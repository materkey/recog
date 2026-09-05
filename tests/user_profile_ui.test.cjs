const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const path = require('node:path');

const source = fs.readFileSync(path.join(__dirname, '../recog/web/static/user-profile.js'), 'utf8');
const html = fs.readFileSync(path.join(__dirname, '../recog/web/static/index.html'), 'utf8');
const payload = (name = '', application_state = 'applied', suggested_name = '') => ({
    profile: { name, aliases: [], configured: !!name, revision: name || 'unconfigured' },
    application_state, suggested_name,
});
const response = (data) => ({ ok: true, json: async () => JSON.parse(JSON.stringify(data)) });
const flush = () => new Promise((resolve) => setImmediate(resolve));

function harness(fetch, { stored = new Map(), storageFails = false } = {}) {
    const elements = new Map();
    const docListeners = {}, windowListeners = {}, timers = new Map();
    const calls = [];
    let timerId = 0;
    const document = {
        hidden: false, activeElement: null,
        addEventListener(name, callback) { docListeners[name] = callback; },
        getElementById(id) { assert.ok(elements.has(id), `Missing markup: ${id}`); return elements.get(id); },
    };
    function element(id) {
        return {
            id, textContent: '', value: '', hidden: true, open: false, disabled: false, isConnected: true,
            listeners: {},
            set innerHTML(_) { throw Error('Profile values must never be interpolated as HTML'); },
            addEventListener(name, callback) { this.listeners[name] = callback; },
            focus() { document.activeElement = this; },
            showModal() { this.open = true; }, close() { this.open = false; },
            querySelector() { return summary; },
        };
    }
    const summary = element('settings-summary');
    for (const [, id] of html.matchAll(/id="(profile-[^"]+)"/g)) elements.set(id, element(id));
    document.activeElement = summary;
    const window = {
        addEventListener(name, callback) { windowListeners[name] = callback; },
        confirm: () => true,
    };
    const context = vm.createContext({
        window, document, AbortController, TypeError,
        fetch: async (...args) => { calls.push(args); return fetch(...args); },
        sessionStorage: {
            getItem(key) { if (storageFails) throw Error('blocked'); return stored.get(key); },
            setItem(key, value) { if (storageFails) throw Error('blocked'); stored.set(key, value); },
        },
        setTimeout(callback, delay) { const id = ++timerId; timers.set(id, { callback, delay }); return id; },
        clearTimeout(id) { timers.delete(id); },
    });
    vm.runInContext(source, context);
    const get = (id) => elements.get('profile-' + id);
    return {
        get, calls, stored, window, document, timers,
        async boot() { docListeners.DOMContentLoaded(); await flush(); },
        async click(id) { get(id).focus(); await get(id).listeners.click(); await flush(); },
        async submit() { await get('form').listeners.submit({ preventDefault() {} }); await flush(); },
        input(id, value) { get(id).value = value; get(id).listeners.input(); },
        async event(name, value = {}) { (docListeners[name] || windowListeners[name])(value); await flush(); },
        async refresh() { await window.userProfile.refresh(); },
        async poll() {
            const item = [...timers.entries()].find(([, timer]) => timer.delay !== 10000);
            assert.ok(item, 'Pending status has a scheduled poll');
            timers.delete(item[0]); item[1].callback(); await flush();
        },
    };
}

test('fresh profile with legacy name - first successful GET - welcome prefills without saving', async () => {
    const h = harness(async () => response(payload('', 'applied', 'Александр')));
    await h.boot();
    assert.equal(h.get('dialog').open, true);
    assert.equal(h.get('title').textContent, 'Как к вам обращаться?');
    assert.equal(h.get('name').value, 'Александр');
    assert.equal(h.document.activeElement, h.get('name'));
    assert.equal(h.calls.length, 1);
    assert.equal(h.calls[0][1].method, undefined);
});

test('welcome with generic name - Later and same-session reload - prompt stays suppressed and setup remains', async () => {
    const stored = new Map();
    const h = harness(async () => response(payload('', 'applied', 'Me')), { stored });
    await h.boot();
    assert.equal(h.get('name').value, '');
    await h.click('later');
    assert.equal(h.get('dialog').open, false);
    assert.equal(h.get('setup').hidden, false);
    const reload = harness(async () => response(payload()), { stored });
    await reload.boot();
    assert.equal(reload.get('dialog').open, false);
    await reload.click('open');
    assert.equal(reload.get('dialog').open, true);
    assert.equal(reload.get('title').textContent, 'Мой профиль');
});

test('never configured and bridge offline - initial pending GET - welcome appears without cleared notice or polling', async () => {
    const h = harness(async () => response(payload('', 'pending')));
    await h.boot();
    assert.equal(h.get('dialog').open, true);
    assert.equal(h.get('setup').hidden, false);
    assert.equal(h.get('notice').hidden, true);
    assert.equal(h.get('dialog-status').textContent, '');
    assert.equal(h.timers.size, 0);
});

test('welcome with storage disabled - Escape then focus - dismissal remains and focus restores', async () => {
    const h = harness(async () => response(payload()), { storageFails: true });
    const previous = h.document.activeElement;
    await h.boot();
    h.get('dialog').listeners.cancel({ preventDefault() {} });
    await h.event('focus');
    assert.equal(h.get('dialog').open, false);
    assert.equal(h.document.activeElement, previous);
});

test('edited name and separate aliases - submit while bridge down - persist once and show pending', async () => {
    let current = payload();
    const h = harness(async (_url, options) => {
        if (options.method === 'PUT') {
            const body = JSON.parse(options.body);
            current = { ...payload(body.name, 'pending'), profile: { ...payload(body.name).profile, aliases: body.aliases } };
        }
        return response(current);
    });
    await h.boot();
    h.input('name', ' Александр ');
    h.input('aliases', 'Саша\nAlex\n');
    await h.submit();
    assert.deepEqual(JSON.parse(h.calls[1][1].body), { name: 'Александр', aliases: ['Саша', 'Alex'] });
    assert.equal(h.get('dialog').open, false);
    assert.match(h.get('notice').textContent, /Профиль сохранён.*ожидает/);
    assert.doesNotMatch(h.get('notice').textContent, /сохранён и применён/);
    current.application_state = 'applied';
    await h.poll();
    assert.match(h.get('notice').textContent, /сохранён и применён/);
    assert.equal(h.timers.size, 0);
});

test('pending profile on Meetings without websocket - hidden then visible - polling pauses and resumes', async () => {
    const h = harness(async () => response(payload('Александр', 'pending')));
    await h.boot();
    assert.equal(h.timers.size, 1);
    h.document.hidden = true;
    await h.event('visibilitychange');
    assert.equal(h.timers.size, 0);
    h.document.hidden = false;
    await h.event('visibilitychange');
    assert.equal(h.timers.size, 1);
    await h.event('pagehide');
    assert.equal(h.timers.size, 0);
    await h.event('pageshow');
    assert.equal(h.timers.size, 1);
});

test('API fails then recovers - focus retry - first successful read still offers welcome', async () => {
    let fail = true;
    const h = harness(async () => { if (fail) throw Error('offline'); return response(payload()); });
    await h.boot();
    assert.equal(h.get('dialog').open, false);
    fail = false;
    await h.event('focus');
    assert.equal(h.get('dialog').open, true);
});

test('save validation failure - submit - draft remains editable with useful error', async () => {
    const h = harness(async (_url, options) => options.method === 'PUT'
        ? { ok: false, status: 422 } : response(payload()));
    await h.boot();
    h.input('name', 'Александр');
    await h.submit();
    assert.equal(h.get('dialog').open, true);
    assert.equal(h.get('name').value, 'Александр');
    assert.equal(h.get('save').disabled, false);
    assert.equal(h.get('error').hidden, false);
    assert.match(h.get('error').textContent, /80 символов/);
});

test('name contains HTML - open and save - name remains literal input data', async () => {
    const name = '<img src=x onerror=alert(1)>';
    const h = harness(async () => response(payload(name)));
    await h.boot();
    await h.click('open');
    assert.equal(h.get('name').value, name);
    await h.submit();
    const put = h.calls.find(([, options]) => options.method === 'PUT');
    assert.equal(JSON.parse(put[1].body).name, name);
});

test('second browser updates profile - focus refresh - reopened form reads current shared values', async () => {
    let current = payload('Старое имя');
    const h = harness(async () => response(current));
    await h.boot();
    current = payload('Новое имя');
    await h.event('focus');
    await h.click('open');
    assert.equal(h.get('name').value, 'Новое имя');
    h.input('name', 'Мой черновик');
    current = payload('Ещё одно имя');
    await h.refresh();
    assert.equal(h.get('name').value, 'Мой черновик');
});

test('configured profile - clear cancelled then confirmed - DELETE requires explicit consent', async () => {
    const cleared = payload('', 'pending');
    cleared.profile.revision = 'cleared';
    const h = harness(async (_url, options) => response(options.method === 'DELETE' ? cleared : payload('Александр')));
    await h.boot();
    await h.click('open');
    assert.equal(h.get('clear').hidden, false);
    h.window.confirm = () => false;
    await h.click('clear');
    assert.equal(h.calls.some(([, options]) => options.method === 'DELETE'), false);
    h.window.confirm = () => true;
    await h.click('clear');
    assert.equal(h.calls.some(([, options]) => options.method === 'DELETE'), true);
    assert.match(h.get('notice').textContent, /Профиль очищен.*ожидает/);
    assert.equal(h.get('setup').hidden, false);
});

test('GET started before save - stale response arrives after PUT - saved revision remains', async () => {
    let resolveOld;
    let gets = 0;
    const h = harness(async (_url, options) => {
        if (options.method === 'PUT') return response(payload('Новое имя', 'pending'));
        if (++gets === 1) return response(payload());
        return new Promise((resolve) => { resolveOld = resolve; });
    });
    await h.boot();
    const reading = h.refresh();
    h.input('name', 'Новое имя');
    await h.submit();
    resolveOld(response(payload('Старое имя')));
    await reading;
    assert.equal(h.get('name').value, 'Новое имя');
    assert.match(h.get('notice').textContent, /ожидает/);
});
