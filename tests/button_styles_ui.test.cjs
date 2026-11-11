const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const staticDir = path.join(__dirname, '../crates/recog-web/static');

function sources(extensions) {
    return fs.readdirSync(staticDir)
        .filter(name => extensions.includes(path.extname(name)))
        .map(name => fs.readFileSync(path.join(staticDir, name), 'utf8'))
        .join('\n');
}

// A `btn-*` class only changes how a button looks, so one that no stylesheet
// selector names does nothing: the button looks like a plain `.btn`.
test('web UI gives a button a btn-* class - stylesheets - some rule names that class', () => {
    const used = new Set(sources(['.html', '.js']).match(/(?<![\w-])btn-[\w-]+/g));
    const styled = new Set(sources(['.css']).replace(/\{[^}]*\}/g, '{}').match(/\.btn-[\w-]+/g));
    const unstyled = [...used].filter(name => !styled.has(`.${name}`)).sort();
    assert.ok(used.has('btn-danger') && used.has('btn-privacy'), 'the scan found no button classes');
    assert.deepEqual(unstyled, []);
});

function channels(color) {
    const hex = /^#([0-9a-f]{6})$/i.exec(color);
    if (hex) return [0, 2, 4].map(i => parseInt(hex[1].slice(i, i + 2), 16)).concat(1);
    const parts = color.match(/[\d.]+/g).map(Number);
    return parts.length === 3 ? parts.concat(1) : parts;
}

function luminance(color) {
    const linear = color.slice(0, 3).map(value => {
        const v = value / 255;
        return v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4;
    });
    return 0.2126 * linear[0] + 0.7152 * linear[1] + 0.0722 * linear[2];
}

function contrast(text, tint, surface) {
    const background = surface.slice(0, 3).map((value, i) => tint[i] * tint[3] + value * (1 - tint[3]));
    const [light, dark] = [luminance(text), luminance(background)].sort((a, b) => b - a);
    return (light + 0.05) / (dark + 0.05);
}

// WCAG 1.4.3: 4.5:1 for text of a button's size. The danger button's background
// is a translucent red, so what is behind it decides: the page or a panel.
test('danger button on the page and on a panel - its label, also under the pointer - contrasts 4.5:1 with its background', () => {
    const css = sources(['.css']);
    const declaration = (selector, property) => {
        const rule = new RegExp(`(?:^|\\}|\\*/)\\s*${selector.replace(/[.:]/g, '\\$&')}\\s*\\{([^}]*)\\}`).exec(css);
        return new RegExp(`(?:^|[\\s;])${property}\\s*:\\s*([^;]+);`).exec(rule[1])[1].trim();
    };
    const text = channels(declaration('.btn-danger', 'color'));
    for (const surface of ['--bg-primary', '--bg-secondary']) {
        for (const selector of ['.btn-danger', '.btn-danger:hover']) {
            const ratio = contrast(text, channels(declaration(selector, 'background')), channels(declaration(':root', surface)));
            assert.ok(ratio >= 4.5, `${selector} over ${surface}: ${ratio.toFixed(2)}:1`);
        }
    }
});
