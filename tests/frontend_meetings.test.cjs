const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const source = fs.readFileSync(path.join(__dirname, '../frontend/static/app.js'), 'utf8');
const CACHE_KEY = 'recog.meetings.v1';
const row = (title) => ({ id: title, title, source: 'ktalk', created_at: '2026-09-01T10:00:00Z' });
const response = (rows) => ({ ok: true, json: async () => rows });

function deferred() {
    let resolve, reject;
    const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
    return { promise, resolve, reject };
}

function harness(fetch, cache = null, storageFails = false) {
    const elements = new Map();
    const stored = new Map(cache === null ? [] : [[CACHE_KEY, cache]]);
    const element = () => ({
        innerHTML: '', textContent: '', style: {}, value: '',
        listeners: {},
        addEventListener(name, callback) { this.listeners[name] = callback; },
        setAttribute() {}, removeAttribute() {},
    });
    const context = vm.createContext({
        console, fetch, setTimeout, clearTimeout,
        localStorage: { getItem: () => null, setItem() {} },
        sessionStorage: {
            getItem(key) { if (storageFails) throw Error('disabled'); return stored.get(key) ?? null; },
            setItem(key, value) { if (storageFails) throw Error('quota'); stored.set(key, value); },
            removeItem(key) { if (storageFails) throw Error('disabled'); stored.delete(key); },
        },
        window: { location: { hash: '#/meeting/detail' } },
        document: {
            addEventListener() {},
            getElementById(id) { if (!elements.has(id)) elements.set(id, element()); return elements.get(id); },
            createElement() { return { textContent: '', get innerHTML() { return this.textContent; } }; },
        },
        DatePicker: { init() {}, getDate: () => '', filterMeetings: (rows) => rows },
    });
    vm.runInContext(source + '\nthis.view = views.meetings; this.api = api;', context);
    return { view: context.view, api: context.api, stored,
        element: (id) => elements.get(id),
        list: () => elements.get('meetings-list').innerHTML,
        status: () => elements.get('meetings-status')?.textContent || '',
    };
}

test('saved list - loading update - cards appear before the response', async () => {
    const pending = deferred();
    const h = harness(() => pending.promise, JSON.stringify({ savedAt: Date.now(), meetings: [row('Saved')] }));
    const loading = h.view.load();
    assert.match(h.list(), /Saved/);
    assert.match(h.status(), /Updating/);
    pending.resolve(response([row('Fresh')]));
    await loading;
    assert.match(h.list(), /Fresh/);
    assert.doesNotMatch(h.list(), /Saved/);
    assert.equal(h.status(), '');
});

test('successful empty list - revisiting - empty state stays visible during refresh', async () => {
    const pending = deferred();
    let calls = 0;
    const h = harness(() => ++calls === 1 ? Promise.resolve(response([])) : pending.promise);
    await h.view.load();
    const loading = h.view.load();
    assert.match(h.list(), /No meetings found/);
    pending.resolve(response([row('Fresh')]));
    await loading;
});

test('saved list - refresh fails - cards remain with an error status', async () => {
    const h = harness(async () => { throw Error('offline'); }, JSON.stringify({ savedAt: Date.now(), meetings: [row('Saved')] }));
    await h.view.load();
    assert.match(h.list(), /Saved/);
    assert.match(h.status(), /Could not refresh/);
});

for (const cache of ['invalid JSON', JSON.stringify({ savedAt: Date.now(), meetings: [null] }),
                     JSON.stringify({ savedAt: Date.now(), meetings: [{ ...row('Bad'), participants: 'wrong' }] }),
                     JSON.stringify({ savedAt: Date.now() - 3600000, meetings: [row('Expired')] })]) {
    test(`invalid or expired cache ${cache.slice(0, 35)} - load - fetch succeeds`, async () => {
        const h = harness(async () => response([row('Fresh')]), cache);
        const loading = h.view.load();
        assert.match(h.list(), /Loading meetings/);
        await loading;
        assert.match(h.list(), /Fresh/);
    });
}

test('saved list - changing source during refresh - filter is already usable', async () => {
    const pending = deferred();
    const h = harness(() => pending.promise, JSON.stringify({ savedAt: Date.now(), meetings: [row('Saved')] }));
    const loading = h.view.load();
    const change = h.element('filter-source').listeners.change;
    assert.equal(typeof change, 'function');
    change({ target: { value: 'matrix' } });
    assert.match(h.list(), /No meetings found/);
    pending.resolve(response([row('Fresh')]));
    await loading;
    assert.match(h.list(), /No meetings found/);
});

test('unavailable browser storage - load - fetched cards still render', async () => {
    const h = harness(async () => response([row('Fresh')]), null, true);
    await h.view.load();
    assert.match(h.list(), /Fresh/);
    assert.equal(h.status(), '');
});

test('two loads - older response arrives last - newest list and cache survive', async () => {
    const first = deferred(), second = deferred();
    let calls = 0;
    const h = harness(() => ++calls === 1 ? first.promise : second.promise);
    const older = h.view.load(), newer = h.view.load();
    second.resolve(response([row('New')]));
    await newer;
    first.resolve(response([row('Old')]));
    await older;
    assert.match(h.list(), /New/);
    assert.doesNotMatch(h.list(), /Old/);
    assert.equal(JSON.parse(h.stored.get(CACHE_KEY)).meetings[0].title, 'New');
});

for (const mutation of ['deleteMeeting', 'generateTitle']) {
    test(`pending GET - ${mutation} succeeds - late GET cannot restore the cache`, async () => {
        const pending = deferred();
        const h = harness((url, options) => options ? Promise.resolve(response({ title: 'Renamed' })) : pending.promise);
        const loading = h.view.load();
        await h.api[mutation]('meeting');
        pending.resolve(response([row('Stale')]));
        await loading;
        assert.equal(h.stored.has(CACHE_KEY), false);
        assert.equal(h.view.meetings.length, 0);
    });
}
