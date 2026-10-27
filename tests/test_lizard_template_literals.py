"""The quality gate's lizard must find function ends past nested template literals."""

import sys
from pathlib import Path

import pytest

lizard = pytest.importorskip("lizard")

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "scripts"))

import lizard_template_literals  # noqa: E402

NESTED = """const views = {
    list: {
        render(results) {
            const container = document.getElementById('results');
            container.innerHTML = results.map(r => `
                <div>${r.title}</div>
                ${r.matches.map(m => `
                    <span>${m.text}</span>
                `).join('')}
                ${r.note ? `<em>${r.note}</em>` : ''}
            `).join('');
        },

        after(value) {
            if (value) {
                return 1;
            }
            return 2;
        },
    },
};
"""


def functions(code):
    analysis = lizard.analyze_file.analyze_source_code("sample.js", code)
    return {
        f.name: (f.start_line, f.end_line, f.cyclomatic_complexity) for f in analysis.function_list
    }


def test_nested_template_literal_when_analyzing_then_each_method_keeps_its_own_lines():
    lizard_template_literals.install()

    found = functions(NESTED)

    assert found["render"][:2] == (3, 12)
    assert found["after"] == (14, 19, 2)


@pytest.mark.parametrize(
    "path",
    [
        "browser-extension/src/ui.js",
        "browser-extension/src/platforms/matrix.js",
        "browser-extension/src/platforms/gmeet.js",
        "browser-extension/src/platforms/zoom.js",
        "browser-extension/src/meeting.js",
    ],
)
def test_source_without_nested_templates_when_tokenizing_then_tokens_are_unchanged(path):
    from lizard_languages.typescript import TypeScriptReader

    code = (ROOT / path).read_text(encoding="utf-8")
    assert "`" in code
    lizard_template_literals.install()

    patched = list(TypeScriptReader.generate_tokens(code))
    original = list(lizard_template_literals.ORIGINAL_GENERATE_TOKENS(code))

    assert patched == original


def test_installed_twice_when_tokenizing_then_patch_is_not_stacked():
    lizard_template_literals.install()
    lizard_template_literals.install()

    assert functions(NESTED)["after"] == (14, 19, 2)
