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
