"""Make lizard find the end of a template literal that nests another one.

lizard 1.24.0 matches a JavaScript/TypeScript template literal with the regex
`.*?` (up to the next backtick). A template inside another's `${...}` ends that
match in the middle of the expression, the braces stop balancing, and the
enclosing function silently absorbs every function after it: the gate measured
47 of the 189 functions of recog/web/static/app.js. This scanner finds the
literal's real end and then yields the same pieces lizard yields for a literal
it reads correctly, so sources without nested templates tokenize exactly as
before. The quality gate installs it at import, which also reaches crapkit's
analysis workers (they import the gate's main module).
"""

import lizard
from lizard_languages.typescript import TypeScriptReader

PATCHED_LIZARD = "1.24.0"
ORIGINAL_GENERATE_TOKENS = TypeScriptReader.generate_tokens


def _skip_string(text: str, start: int) -> int:
    """Index after the quoted string or template literal that starts at `start`."""
    quote = text[start]
    i = start + 1
    while i < len(text):
        if text[i] == "\\":
            i += 2
            continue
        if text[i] == quote:
            return i + 1
        if quote == "`" and text.startswith("${", i):
            i = _skip_expression(text, i + 2)
            continue
        i += 1
    return len(text)


def _skip_expression(text: str, start: int) -> int:
    """Index after the `}` that closes a `${` whose body starts at `start`."""
    depth = 1
    i = start
    while i < len(text):
        char = text[i]
        if char in "'\"`":
            i = _skip_string(text, i)
            continue
        if char == "{":
            depth += 1
        elif char == "}":
            depth -= 1
            if depth == 0:
                return i + 1
        i += 1
    return len(text)


def _template_pieces(literal: str):
    """lizard's own pieces: quote, literal text, `${`, raw expression, `}`."""
    yield "`"
    content = literal[1:-1]
    i = 0
    while True:
        start = i
        while i < len(content) and not content.startswith("${", i):
            i += 2 if content[i] == "\\" else 1
        if i > start:
            yield "`" + content[start:i] + "`"
        if i >= len(content):
            break
        end = _skip_expression(content, i + 2)
        yield "${"
        yield content[i + 2 : end - 1]
        yield "}"
        i = end
    yield "`"


def _template_spans(code: str):
    """(start, end) of each outermost template literal outside comments and strings."""
    i = 0
    while i < len(code):
        if code.startswith("//", i):
            end = code.find("\n", i)
            i = len(code) if end == -1 else end
        elif code.startswith("/*", i):
            end = code.find("*/", i + 2)
            i = len(code) if end == -1 else end + 2
        elif code[i] in "'\"":
            i = _skip_string(code, i)
        elif code[i] == "`":
            end = _skip_string(code, i)
            yield i, end
            i = end
        else:
            i += 1


def generate_tokens(source_code, addition="", token_class=None):
    position = 0
    for start, end in _template_spans(source_code):
        yield from ORIGINAL_GENERATE_TOKENS(source_code[position:start], addition, token_class)
        yield from _template_pieces(source_code[start:end])
        position = end
    yield from ORIGINAL_GENERATE_TOKENS(source_code[position:], addition, token_class)


def install() -> None:
    if lizard.version != PATCHED_LIZARD:
        raise RuntimeError(
            f"scripts/lizard_template_literals.py targets lizard {PATCHED_LIZARD}, "
            f"found {lizard.version}: check whether the new version reads nested "
            "template literals, then update or remove the patch"
        )
    TypeScriptReader.generate_tokens = staticmethod(generate_tokens)
