const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const path = require('node:path');

const sourcePath = path.join(__dirname, '../crates/recog-web/static/app.js');
const source = fs.readFileSync(sourcePath, 'utf8');
const response = (enabled, configured = true) => ({ ok: true, json: async () => ({ enabled, configured }) });

function harness(fetch, cached = 'false') {
    const buttons = new Map(['privacy-mode-btn', 'live-privacy-btn'].map(id => [id, {
        disabled: false, active: false,
        classList: { toggle(_name, on) { buttons.get(id).active = on; } },
    }]));
    const alerts = [], calls = [];
    const context = vm.createContext({
        console: { error() {} },
        fetch: async (...args) => { calls.push(args); return fetch(...args); },
        localStorage: { getItem: () => cached, setItem(_key, value) { cached = String(value); } },
        window: {},
        document: { addEventListener() {}, getElementById: id => buttons.get(id) },
        alert: message => alerts.push(message),
    });
    vm.runInContext(source, context, { filename: sourcePath });
    return { context, buttons, alerts, calls, cached: () => cached };
}

test('cached privacy on - first server sync - migrate preference and enable both buttons', async () => {
    const h = harness(async (_url, options) => options.method ? response(true) : response(false, false), 'true');
    await h.context.loadPrivacy();
    assert.equal(h.calls.length, 2);
    assert.deepEqual(JSON.parse(h.calls[1][1].body), { enabled: true });
    for (const button of h.buttons.values()) {
        assert.equal(button.active, true);
        assert.equal(button.disabled, false);
    }
});

test('server privacy on - stale browser cache off - server preference wins', async () => {
    const h = harness(async () => response(true));
    await h.context.loadPrivacy();
    assert.equal(h.calls.length, 1);
    assert.equal(h.cached(), 'true');
});

test('failed save - toggle - preserve acknowledged preference and report failure', async () => {
    const h = harness(async (_url, options) => options.method ? { ok: false } : response(false));
    await h.context.loadPrivacy();
    await h.context.togglePrivacy();
    assert.equal(h.cached(), 'false');
    assert.equal(h.buttons.get('privacy-mode-btn').active, false);
    assert.equal(h.alerts.length, 1);
});

test('save pending - repeated click - send only one mutation and wait for acknowledgement', async () => {
    let resolve;
    const pending = new Promise(done => { resolve = done; });
    const h = harness(async (_url, options) => options.method ? pending : response(false));
    await h.context.loadPrivacy();
    const saving = h.context.togglePrivacy();
    await h.context.togglePrivacy();
    assert.equal(h.calls.length, 2);
    assert.equal(h.cached(), 'false');
    assert.equal(h.buttons.get('privacy-mode-btn').disabled, true);
    resolve(response(true));
    await saving;
    assert.equal(h.cached(), 'true');
    assert.equal(h.buttons.get('live-privacy-btn').active, true);
});

test('old GET pending - toggle succeeds first - delayed GET cannot undo privacy', async () => {
    let resolve;
    const pending = new Promise(done => { resolve = done; });
    let reads = 0;
    const h = harness(async (_url, options) => options.method ? response(true) : (++reads === 1 ? response(false) : pending));
    await h.context.loadPrivacy();
    const loading = h.context.loadPrivacy();
    await h.context.togglePrivacy();
    resolve(response(false));
    await loading;
    assert.equal(h.cached(), 'true');
});

test('privacy on - button marked active - stylesheet paints it differently from the inactive one', () => {
    const css = fs.readFileSync(path.join(__dirname, '../crates/recog-web/static/style.css'), 'utf8');
    const rule = /\.btn-privacy\.active\s*\{([^}]*)\}/.exec(css);
    assert.ok(rule, 'style.css has no rule for .btn-privacy.active');
    assert.match(rule[1], /background\s*:/);
});
