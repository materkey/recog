const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const css = fs.readFileSync(path.join(__dirname, '../crates/recog-web/static/style.css'), 'utf8');

// Sources are an open set (importers bring their own), so the badge cannot rely
// on a `.source-<name>` rule existing for each of them.
test('meeting of a source without a colour of its own - source badge - has the default background and colour', () => {
    const rule = /(?:^|\})\s*\.meeting-source\s*\{([^}]*)\}/.exec(css);
    assert.ok(rule, 'style.css has no rule for .meeting-source');
    assert.match(rule[1], /background\s*:/);
    assert.match(rule[1], /(?:^|[\s;])color\s*:/);
});
