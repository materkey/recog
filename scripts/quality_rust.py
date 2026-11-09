"""The production part of the Rust workspace and its line coverage, for the quality gate.

Production Rust is `rust/<crate>/src/**/*.rs` without its test code. Test code is
found in the syntax tree (tree-sitter-rust, pinned), not by name: an item carrying
`#[test]` (or `#[<runner>::test]`) or a `#[cfg(...)]` that cannot hold without
`test` is blanked out of the source, keeping every line where it was, and a file
that is a module declared under such an attribute (`#[cfg(test)] mod tests;`),
or a module of one, is left out whole. The crate's `tests/`, `benches/` and
build script sit outside `src/` and are never read. Function signatures without
a body (trait methods, extern fns) are blanked too: lizard 1.24.0 does not end a
function at `;` and would join each to the next body.

Coverage comes from `cargo llvm-cov`'s lcov export. A function's coverage is
that of the lines (and branches, when the toolchain records them) inside its
span that belong to no function nested in it, the same attribution crapkit
applies to Istanbul reports.
"""

import re
from pathlib import PurePosixPath

# Files whose `mod x;` lives beside them, not in a directory named after them.
MODULE_DIRECTORY_FILES = ("lib.rs", "main.rs", "mod.rs")
_CFG_TOKEN = re.compile(r'\s*(?:(\w+)|("(?:[^"\\]|\\.)*")|([(),=]))')
_SKIPPED = {"attribute_item", "line_comment", "block_comment"}


def is_production_path(name):
    """`rust/<crate>/src/**/*.rs`: the crate's own sources, not tests/, benches/ or build.rs."""
    parts = PurePosixPath(name).parts
    return len(parts) >= 4 and parts[0] == "rust" and parts[2] == "src" and name.endswith(".rs")


def _cfg_tokens(text):
    tokens, position = [], 0
    text = text.strip()
    while position < len(text):
        match = _CFG_TOKEN.match(text, position)
        if match is None or match.end() == position:
            raise ValueError(f"Unreadable cfg predicate: {text!r}")
        tokens.append(match.group(match.lastindex))
        position = match.end()
    return tokens


def _cfg_predicate(tokens, position):
    """(predicate, next position); a predicate is a name, name = "value" or name(list)."""
    name = tokens[position]
    if not re.fullmatch(r"\w+", name):
        raise ValueError(f"Malformed cfg predicate at {name!r}")
    position += 1
    if position < len(tokens) and tokens[position] == "=":
        return (name, tokens[position + 1]), position + 2
    if position < len(tokens) and tokens[position] == "(":
        children, position = [], position + 1
        while tokens[position] != ")":
            child, position = _cfg_predicate(tokens, position)
            children.append(child)
            if tokens[position] == ",":
                position += 1
        return (name, children), position + 1
    return name, position


def _needs_test(predicate):
    if predicate == "test":
        return True
    if isinstance(predicate, str) or not isinstance(predicate[1], list):
        return False
    name, children = predicate
    if name == "all":
        return any(_needs_test(child) for child in children)
    if name == "any":
        return bool(children) and all(_needs_test(child) for child in children)
    return False  # not(…) and anything else can hold without test


def requires_test(cfg_arguments):
    """Whether `cfg(<arguments>)` is false in every build without `--cfg test`."""
    tokens = _cfg_tokens(cfg_arguments)
    if tokens[:1] != ["("] or tokens[-1:] != [")"]:
        raise ValueError(f"Malformed cfg arguments: {cfg_arguments!r}")
    predicate, position = _cfg_predicate(tokens, 1)
    if position != len(tokens) - 1:
        raise ValueError(f"Malformed cfg arguments: {cfg_arguments!r}")
    return _needs_test(predicate)


def is_test_attribute(text):
    """`#[test]`, `#[tokio::test(...)]`, or a `#[cfg(...)]` that needs test."""
    match = re.fullmatch(r"#!?\[\s*([\w:\s]+?)\s*(\(.*\))?\s*\]", text, re.DOTALL)
    if match is None:
        return False
    path = re.sub(r"\s", "", match.group(1))
    if path == "test" or path.endswith("::test"):
        return True
    return path == "cfg" and match.group(2) is not None and requires_test(match.group(2))


def _parser():
    import tree_sitter
    import tree_sitter_rust

    return tree_sitter.Parser(tree_sitter.Language(tree_sitter_rust.language()))


def _name(node):
    name = node.child_by_field_name("name")
    return name.text.decode() if name is not None else None


def _path_attribute(item):
    node = item.prev_named_sibling
    while node is not None and node.type in _SKIPPED:
        if node.type == "attribute_item":
            match = re.fullmatch(r'#\[\s*path\s*=\s*"([^"]*)"\s*\]', node.text.decode())
            if match:
                return match.group(1)
        node = node.prev_named_sibling
    return None


def scan(code):
    """(whole file is test code, byte spans to blank, declared file modules).

    A declared module is (inline module path, name, #[path], declared under test).
    """
    tree = _parser().parse(code.encode())
    if tree.root_node.has_error:
        raise ValueError("Rust source does not parse")
    spans, modules, whole = [], [], False

    def walk(node, inline, in_test):
        nonlocal whole
        for child in node.named_children:
            if child.type == "inner_attribute_item" and is_test_attribute(child.text.decode()):
                if node.type == "source_file":
                    whole = True
                else:
                    spans.append((node.start_byte, node.end_byte))
                    in_test = True
        attributes = []
        for child in node.named_children:
            if child.type == "attribute_item":
                attributes.append(child)
                continue
            if child.type in _SKIPPED:
                continue
            test = in_test
            if any(is_test_attribute(a.text.decode()) for a in attributes):
                spans.append((attributes[0].start_byte, child.end_byte))
                test = True
            attributes = []
            if child.type == "function_signature_item":
                # A trait method or extern fn without a body: see the module docstring.
                spans.append((child.start_byte, child.end_byte))
                continue
            if child.type == "mod_item":
                body = child.child_by_field_name("body")
                if body is None:
                    modules.append((inline, _name(child), _path_attribute(child), test))
                else:
                    walk(body, (*inline, _name(child)), test)
                continue
            walk(child, inline, test)

    walk(tree.root_node, (), False)
    return whole, spans, modules


def blank(code, spans):
    """The source with every span replaced by spaces; line numbers and other bytes stay."""
    data = bytearray(code.encode())
    for start, end in spans:
        for index in range(start, end):
            if data[index] != ord("\n"):
                data[index] = ord(" ")
    return data.decode()


def lizard_text(code):
    """The source as lizard 1.24.0 can tokenize it; every line and function stays where it was.

    Its Rust tokenizer reads `'_'` and `'a'` as a lifetime and an opening quote, which
    then swallows code up to the next quote, and a `#` as a C preprocessor line, so a
    multi-line `r#"…"#` leaks its text as code: either moves or merges functions. Char
    literals keep their quotes around dots; raw strings become plain strings of spaces.
    """
    tree = _parser().parse(code.encode())
    data = bytearray(code.encode())
    stack = [tree.root_node]
    while stack:
        node = stack.pop()
        if node.type == "char_literal":
            first = data.index(ord("'"), node.start_byte)
            for index in range(first + 1, node.end_byte - 1):
                data[index] = ord(".")
        elif node.type == "raw_string_literal":
            for index in range(node.start_byte, node.end_byte):
                if data[index] != ord("\n"):
                    data[index] = ord(" ")
            data[node.start_byte] = data[node.end_byte - 1] = ord('"')
        else:
            stack.extend(node.children)
    return data.decode()


def check_functions(name, code, functions):
    """Refuse lizard's functions (name, start, end) unless the syntax tree has the same ones.

    The tree's `fn` items with a body, by name and last line; lizard alone may also
    report the functions it reads in a `macro_rules!` template, which the tree keeps
    as tokens.
    """
    tree = _parser().parse(code.encode())
    expected, macros, stack = set(), [], [tree.root_node]
    while stack:
        node = stack.pop()
        if node.type == "function_item":
            expected.add((_name(node), node.end_point[0] + 1))
        elif node.type == "macro_definition":
            macros.append((node.start_point[0] + 1, node.end_point[0] + 1))
        stack.extend(node.named_children)
    found = {(function, end) for function, _, end in functions}
    missing = expected - found
    extra = {
        (function, end)
        for function, start, end in functions
        if (function, end) not in expected
        and not any(first <= start and end <= last for first, last in macros)
    }
    if missing or extra:
        raise ValueError(
            f"lizard misread {name}: functions it missed {sorted(missing)[:5]}, "
            f"functions the syntax tree lacks {sorted(extra)[:5]}"
        )


def _module_directory(name):
    path = PurePosixPath(name)
    root = (
        path.name in MODULE_DIRECTORY_FILES
        or path.parent.name == "bin"
        and path.parent.parent.name == "src"
    )
    return path.parent if root else path.parent / path.stem


def module_file(declaring, inline, name, path_attribute, available):
    """The file of `mod name;` declared in `declaring` inside inline modules `inline`."""
    if path_attribute is not None:
        base = PurePosixPath(declaring).parent if not inline else _module_directory(declaring)
        candidates = [str(base.joinpath(*inline, path_attribute))]
    else:
        base = _module_directory(declaring).joinpath(*inline)
        candidates = [str(base / f"{name}.rs"), str(base / name / "mod.rs")]
    found = [candidate for candidate in candidates if candidate in available]
    if len(found) != 1:
        raise ValueError(f"Cannot resolve module {name} declared in {declaring}: {candidates}")
    return found[0]


def production_sources(files):
    """{path: source with test code blanked} for the production files among `files`.

    `files` maps every `.rs` file of a crate's `src/` to its text; test-only
    files are left out.
    """
    scanned = {name: scan(code) for name, code in files.items()}
    test_files = {name for name, (whole, _, _) in scanned.items() if whole}
    pending = []
    for name, (_, _, modules) in scanned.items():
        for inline, module, path, test in modules:
            if test:
                pending.append(module_file(name, inline, module, path, files))
    while pending:
        name = pending.pop()
        if name in test_files:
            continue
        test_files.add(name)
        for inline, module, path, _ in scanned[name][2]:
            pending.append(module_file(name, inline, module, path, files))
    return {
        name: blank(files[name], scanned[name][1])
        for name in sorted(files)
        if name not in test_files
    }


def _count(value, field):
    if not re.fullmatch(r"\d+", value):
        raise ValueError(f"lcov {field} is not a count: {value!r}")
    return int(value)


def parse_lcov(text):
    """{source file: ({line: hits}, {line: [taken per branch]})} from an lcov export."""
    result, current = {}, None
    for number, raw in enumerate(text.splitlines(), 1):
        line = raw.strip()
        if not line:
            continue
        tag, _, value = line.partition(":")
        if tag == "TN":
            continue
        if tag == "SF":
            if current is not None or not value:
                raise ValueError(f"lcov line {number}: SF inside a record")
            current = result.setdefault(value, ({}, {}))
        elif line == "end_of_record":
            if current is None:
                raise ValueError(f"lcov line {number}: end_of_record outside a record")
            current = None
        elif current is None:
            raise ValueError(f"lcov line {number}: {tag} outside a record")
        elif tag == "DA":
            fields = value.split(",")
            if len(fields) < 2:
                raise ValueError(f"lcov line {number}: malformed DA")
            row = _count(fields[0], "line")
            current[0][row] = max(current[0].get(row, 0), _count(fields[1], "hits"))
        elif tag == "BRDA":
            fields = value.split(",")
            if len(fields) != 4:
                raise ValueError(f"lcov line {number}: malformed BRDA")
            taken = 0 if fields[3] == "-" else _count(fields[3], "branch hits")
            current[1].setdefault(_count(fields[0], "line"), []).append(taken)
    if current is not None:
        raise ValueError("lcov export ends inside a record")
    return result


def function_coverage(functions, lines, branches):
    """[(function, lines, hit lines, branches, taken branches)] for one file's functions.

    `functions` are (start, end) spans; each measured line counts for the innermost
    span holding it (the shortest; the later start on a tie).
    """
    counts = [[0, 0, 0, 0] for _ in functions]
    order = sorted(
        range(len(functions)),
        key=lambda index: (functions[index][1] - functions[index][0], -functions[index][0]),
    )

    def owner(row):
        return next(
            (index for index in order if functions[index][0] <= row <= functions[index][1]),
            None,
        )

    for row, hits in lines.items():
        index = owner(row)
        if index is not None:
            counts[index][0] += 1
            counts[index][1] += hits > 0
    for row, taken in branches.items():
        index = owner(row)
        if index is not None:
            counts[index][2] += len(taken)
            counts[index][3] += sum(1 for value in taken if value > 0)
    return [(function, *count) for function, count in zip(functions, counts)]
