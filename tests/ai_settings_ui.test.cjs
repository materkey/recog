const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const path = require('node:path');

const source = fs.readFileSync(path.join(__dirname, '../recog/web/static/ai-settings.js'), 'utf8');
const html = fs.readFileSync(path.join(__dirname, '../recog/web/static/index.html'), 'utf8');
const copy = (value) => JSON.parse(JSON.stringify(value));
const response = (data, status = 200) => ({ ok: status < 400, status, json: async () => copy(data) });
const task = (id, fields = {}) => ({
    id, label: id, prompt: 'Analyze {transcript}', default_prompt: 'Analyze {transcript}',
    providers: ['claude', 'codex'], allowed_providers: ['claude', 'codex'], multiple: true,
    models: { claude: 'claude-default', codex: 'codex-default' },
    model_overrides: {}, reasoning_effort: 'xhigh', placeholders: ['transcript', 'user_name'],
    required_placeholders: ['transcript'], apply_mode: 'next_request', customized: false, revision: 'r1', ...fields,
});
const payload = (revision = 'r1') => ({
    revision, models: { claude: 'claude-default', codex: 'codex-default' }, model_overrides: {},
    tasks: [task('analysis.increment'), task('title', { providers: ['claude'], multiple: false }),
        task('saved:custom-id', { label: '<img src=x onerror=alert(1)>', providers: ['codex'], multiple: false }),
        task('copilot', { providers: ['claude'], allowed_providers: ['claude'], multiple: false, placeholders: [], required_placeholders: [] })],
    copilot_application: { state: 'pending', model_application: 'next_session' },
});
const flush = () => new Promise((resolve) => setImmediate(resolve));

function harness(fetch) {
    const elements = new Map();
    const docListeners = {}, windowListeners = {};
    const calls = [], confirmations = [];
    const timers = new Map();
    let timerId = 0;
    const document = {
        hidden: false, activeElement: null,
        addEventListener(name, callback) { docListeners[name] = callback; },
        getElementById(id) { assert.ok(elements.has(id), `Missing element ${id}`); return elements.get(id); },
        createElement(tag) { return element('', tag); },
    };
    function element(id, tag = 'div') {
        let elementId = id;
        const node = {
            tag, children: [], textContent: '', value: '', hidden: false, open: false, disabled: false, isConnected: true,
            checked: false, listeners: {},
            get id() { return elementId; },
            set id(value) { elementId = value; elements.set(value, this); },
            set innerHTML(_) { throw Error('Settings must never interpolate HTML'); },
            addEventListener(name, callback) { this.listeners[name] = callback; },
            append(...nodes) { this.children.push(...nodes); },
            replaceChildren(...nodes) { this.children = nodes; },
            focus() { document.activeElement = this; },
            showModal() { this.open = true; }, close() { this.open = false; },
            querySelector() { return summary; },
        };
        return node;
    }
    const summary = element('summary');
    for (const [, id] of html.matchAll(/id="((?:ai-settings-|profile-settings)[^"]*)"/g)) elements.set(id, element(id));
    document.activeElement = summary;
    const window = {
        addEventListener(name, callback) { windowListeners[name] = callback; },
        confirm(message) { confirmations.push(message); return true; },
    };
    const context = vm.createContext({
        window, document, AbortController,
        fetch: async (...args) => { calls.push(args); return fetch(...args); },
        setTimeout(callback, delay) { const id = ++timerId; timers.set(id, { callback, delay }); return id; },
        clearTimeout(id) { timers.delete(id); },
    });
    vm.runInContext(source, context);
    const get = (id) => elements.get('ai-settings-' + id);
    return {
        get, calls, confirmations, window, document, summary, timers,
        async boot() { docListeners.DOMContentLoaded(); await flush(); },
        async click(id) { get(id).focus(); await get(id).listeners.click(); await flush(); },
        input(id, value) { get(id).value = value; get(id).listeners.input(); },
        async select(id, value) { get(id).value = value; await get(id).listeners.change(); await flush(); },
        async event(name, value = {}) { await (docListeners[name] || windowListeners[name])(value); await flush(); },
        provider(name) { return get('providers').children.map((label) => label.children[0]).find((input) => input.value === name); },
        async poll() {
            assert.equal(timers.size, 1, 'One idle pending-status poll is scheduled');
            const [id, timer] = [...timers][0];
            timers.delete(id); timer.callback(); await flush();
            return timer.delay;
        },
    };
}

test('closed settings - page load then open - fetch only on demand and render dynamic text safely', async () => {
    const h = harness(async () => response(payload()));
    await h.boot();
    assert.equal(h.calls.length, 0);
    await h.click('open');
    assert.equal(h.calls.length, 1);
    assert.equal(h.calls[0][0], '/api/ai-settings');
    assert.equal(h.get('dialog').open, true);
    assert.equal(h.get('task').children[2].textContent, '<img src=x onerror=alert(1)>');
    assert.equal(h.get('global-claude').value, '');
    assert.equal(h.get('global-claude').placeholder, 'claude-default');
    assert.equal(h.get('model-codex').placeholder, 'codex-default');
    assert.equal(h.provider('claude').type, 'checkbox');
    assert.equal(h.get('reasoning-row').hidden, false);
    assert.equal(h.document.activeElement, h.get('task'));
});

test('task and global drafts - saving task - encoded id and original revision sent while global draft survives', async () => {
    const updated = payload('r2');
    updated.tasks[2].prompt = '<script>ignored()</script> {transcript}';
    updated.tasks[2].customized = true;
    const h = harness(async (_url, options) => response(options.method ? updated : payload()));
    await h.boot(); await h.click('open');
    await h.select('task', 'saved:custom-id');
    assert.equal(h.provider('codex').type, 'radio');
    h.input('global-claude', 'claude-new');
    h.input('prompt', updated.tasks[2].prompt);
    h.input('model-codex', 'gpt-6-astra');
    await h.select('reasoning', 'high');
    await h.click('save-task');
    assert.equal(h.calls[1][0], '/api/ai-settings/tasks/saved%3Acustom-id');
    const sent = JSON.parse(h.calls[1][1].body);
    assert.deepEqual(sent, { prompt: updated.tasks[2].prompt, providers: ['codex'], models: { claude: '', codex: 'gpt-6-astra' }, reasoning_effort: 'high', claude_effort: '', revision: 'r1' });
    assert.equal(h.get('global-claude').value, 'claude-new');
    assert.match(h.get('provenance').textContent, /вручную/);
    await h.click('save-models');
    assert.equal(JSON.parse(h.calls[2][1].body).revision, 'r2');
    assert.equal(JSON.parse(h.calls[2][1].body).models.claude, 'claude-new');
});

test('Codex effort - choose Auto and save - empty override reaches API', async () => {
    const h = harness(async () => response(payload()));
    await h.boot(); await h.click('open');
    await h.select('reasoning', '');
    await h.click('save-task');
    assert.equal(JSON.parse(h.calls[1][1].body).reasoning_effort, '');
});

test('task draft - saving global defaults - task text survives and inherited model hint updates', async () => {
    const updated = payload('r2');
    updated.models.codex = 'gpt-6-astra'; updated.model_overrides.codex = 'gpt-6-astra';
    const h = harness(async (_url, options) => response(options.method ? updated : payload()));
    await h.boot(); await h.click('open');
    h.input('prompt', 'Local {transcript}'); h.input('global-codex', 'gpt-6-astra');
    await h.click('save-models');
    assert.equal(h.get('prompt').value, 'Local {transcript}');
    assert.equal(h.get('model-codex').placeholder, 'gpt-6-astra');
    assert.equal(h.get('global-codex').value, 'gpt-6-astra');
});

test('Claude and Codex configured - changing effort then saving - independent values survive', async () => {
    const current = payload();
    current.tasks[0].claude_effort = 'high';
    const h = harness(async () => response(current));
    await h.boot(); await h.click('open');
    assert.equal(h.get('claude-effort-row').hidden, false);
    assert.equal(h.get('claude-effort').value, 'high');
    await h.select('claude-effort', 'max');
    await h.click('save-task');
    const sent = JSON.parse(h.calls[1][1].body);
    assert.equal(sent.claude_effort, 'max');
    assert.equal(sent.reasoning_effort, 'xhigh');
    await h.select('task', 'saved:custom-id');
    assert.equal(h.get('claude-effort-row').hidden, true);
    await h.select('task', 'copilot');
    assert.equal(h.get('claude-effort-row').hidden, false);
    assert.equal(h.get('reasoning-row').hidden, true);
    await h.select('claude-effort', 'low');
    await h.click('save-task');
    const copilot = JSON.parse(h.calls.at(-1)[1].body);
    assert.equal(copilot.claude_effort, 'low');
    assert.equal(Object.hasOwn(copilot, 'reasoning_effort'), false);
});

test('concurrent browser write - HTTP 409 - preserve draft and require explicit reload before saving', async () => {
    let current = payload();
    const h = harness(async (_url, options) => options.method ? response({}, 409) : response(current));
    await h.boot(); await h.click('open');
    h.input('prompt', 'My draft {transcript}');
    await h.click('save-task');
    assert.equal(h.get('conflict').hidden, false);
    assert.equal(h.get('prompt').value, 'My draft {transcript}');
    assert.equal(h.get('save-task').disabled, true);
    current = payload('r2'); current.tasks[0].prompt = 'Other browser {transcript}';
    h.window.confirm = () => false;
    await h.click('reload');
    assert.equal(h.calls.length, 2);
    h.window.confirm = () => true;
    await h.click('reload');
    assert.equal(h.get('prompt').value, 'Other browser {transcript}');
    assert.equal(h.get('conflict').hidden, true);
    assert.equal(h.get('save-task').disabled, false);
});

test('dirty draft - focus discovers new revision - draft kept and no write occurs', async () => {
    let current = payload();
    const h = harness(async () => response(current));
    await h.boot(); await h.click('open'); h.input('prompt', 'Mine {transcript}');
    current = payload('r2');
    await h.event('focus');
    assert.equal(h.get('conflict').hidden, false);
    assert.equal(h.get('prompt').value, 'Mine {transcript}');
    assert.equal(h.calls.filter(([, options]) => options.method).length, 0);
});

test('clean draft - focus refresh - latest prompt replaces form without confirmation', async () => {
    let current = payload();
    const h = harness(async () => response(current));
    await h.boot(); await h.click('open');
    current = payload('r2'); current.tasks[0].prompt = 'New {transcript}';
    await h.event('focus');
    assert.equal(h.get('prompt').value, 'New {transcript}');
    assert.equal(h.confirmations.length, 0);
});

test('dirty task - decline switch, Escape and reload - preserve task and focus then confirmed close restores menu', async () => {
    const h = harness(async () => response(payload()));
    await h.boot(); await h.click('open'); h.input('prompt', 'My {transcript}');
    h.window.confirm = () => false;
    await h.select('task', 'copilot');
    assert.equal(h.get('task').value, 'analysis.increment');
    let prevented = false;
    h.get('dialog').listeners.cancel({ preventDefault() { prevented = true; } });
    assert.equal(prevented, true); assert.equal(h.get('dialog').open, true);
    await h.event('beforeunload', { preventDefault() { prevented = true; } });
    h.window.confirm = () => true;
    await h.click('close');
    assert.equal(h.get('dialog').open, false);
    assert.equal(h.document.activeElement, h.summary);
});

test('customized task - confirmed reset - DELETE uses revision and preserves global draft', async () => {
    const h = harness(async () => response(payload()));
    await h.boot(); await h.click('open'); await h.select('task', 'saved:custom-id');
    h.input('global-claude', 'claude-new');
    h.window.confirm = () => false; await h.click('reset'); assert.equal(h.calls.length, 1);
    h.window.confirm = () => true; await h.click('reset');
    assert.equal(h.calls[1][0], '/api/ai-settings/tasks/saved%3Acustom-id?revision=r1');
    assert.equal(h.calls[1][1].method, 'DELETE');
    assert.equal(h.get('global-claude').value, 'claude-new');
});

test('Copilot - select scenario - show application timing', async () => {
    const h = harness(async () => response(payload()));
    await h.boot(); await h.click('open');
    await h.select('task', 'copilot');
    assert.equal(h.get('reasoning-row').hidden, true);
    assert.equal(h.get('providers').children.length, 1);
    assert.match(h.get('timing').textContent, /новой сессии/);
    assert.match(h.get('timing').textContent, /ожидает подключения/);
    assert.match(h.get('placeholders').textContent, /буквально/);
});

test('validation error containing markup - save rejected - message rendered as text and draft retained', async () => {
    const h = harness(async (_url, options) => options.method ? response({ detail: '<script>bad()</script> missing transcript' }, 422) : response(payload()));
    await h.boot(); await h.click('open'); h.input('prompt', 'Missing placeholder'); await h.click('save-task');
    assert.equal(h.get('error').textContent, '<script>bad()</script> missing transcript');
    assert.equal(h.get('prompt').value, 'Missing placeholder');
    assert.equal(h.get('save-task').disabled, false);
});

test('pending read - duplicate focus and Save - single read and no mutation', async () => {
    let resolve;
    const h = harness(() => new Promise((done) => { resolve = done; }));
    await h.boot();
    const opening = h.click('open'); await flush();
    await h.event('focus'); await h.click('save-task');
    assert.equal(h.calls.length, 1);
    assert.equal(h.get('save-task').disabled, true);
    resolve(response(payload())); await opening;
    assert.equal(h.get('save-task').disabled, false);
});

test('failed conflict reload - draft and revision - remain preserved until successful reload', async () => {
    let failing = false;
    const h = harness(async (_url, options) => {
        if (options.method) return response({}, 409);
        if (failing) throw new Error('Network failure');
        return response(payload());
    });
    await h.boot(); await h.click('open'); h.input('prompt', 'Keep {transcript}');
    await h.click('save-task'); failing = true; await h.click('reload');
    assert.equal(h.get('prompt').value, 'Keep {transcript}');
    assert.equal(h.get('conflict').hidden, false);
    assert.equal(h.get('save-task').disabled, true);
});

test('discarded draft - reopen read fails - old draft cannot be saved with a stale snapshot', async () => {
    let failing = false;
    const h = harness(async () => {
        if (failing) throw new Error('Network failure');
        return response(payload());
    });
    await h.boot(); await h.click('open'); h.input('prompt', 'Abandoned {transcript}');
    await h.click('close'); failing = true; await h.click('open');
    assert.equal(h.get('prompt').value, 'Analyze {transcript}');
    assert.equal(h.get('save-task').disabled, true);
    await h.click('save-task');
    assert.equal(h.calls.length, 2);
});

test('no selected providers - save - local validation prevents empty provider request', async () => {
    const h = harness(async () => response(payload()));
    await h.boot(); await h.click('open');
    h.provider('claude').checked = false; h.provider('codex').checked = false;
    h.provider('codex').listeners.change();
    await h.click('save-task');
    assert.equal(h.calls.length, 1);
    assert.match(h.get('error').textContent, /Выберите/);
});

test('pending saved Copilot policy on Meetings - reconnect ack arrives during poll - status updates without focus', async () => {
    let current = payload('r1');
    const h = harness(async () => response(current));
    await h.boot(); await h.click('open'); await h.select('task', 'copilot');
    h.input('prompt', 'Updated policy');
    assert.equal(h.timers.size, 0, 'Typing pauses polling');
    current = payload('r2'); current.tasks[3].prompt = 'Updated policy';
    await h.click('save-task');
    const savedRequest = JSON.parse(h.calls.find(([, options]) => options.method === 'PUT')[1].body);
    assert.equal(Object.hasOwn(savedRequest, 'reasoning_effort'), false, 'Copilot rejects Codex reasoning settings');
    assert.match(h.get('timing').textContent, /ожидает подключения/);
    assert.equal(await h.poll(), 5000);
    assert.equal(await h.poll(), 10000);
    assert.equal(await h.poll(), 20000);
    assert.equal(await h.poll(), 30000);
    current.copilot_application.state = 'applied';
    assert.equal(await h.poll(), 30000);
    assert.doesNotMatch(h.get('timing').textContent, /ожидает подключения/);
    assert.doesNotMatch(h.get('status').textContent, /ожидает подключения/);
    assert.equal(h.timers.size, 0, 'Applied policy stops polling');
    assert.equal(h.calls.filter(([, options]) => options.method).length, 1, 'Status polling never saves settings');
});

test('pending settings - hidden tab, draft, conflict and closed dialog - no background polling', async () => {
    let current = payload('r1');
    const h = harness(async () => response(current));
    await h.boot(); await h.click('open');
    assert.equal(h.timers.size, 1);
    h.document.hidden = true; await h.event('visibilitychange');
    assert.equal(h.timers.size, 0);
    h.document.hidden = false; await h.event('visibilitychange');
    assert.equal(h.timers.size, 1);
    h.input('global-claude', 'Draft');
    assert.equal(h.timers.size, 0);
    current = payload('r2'); await h.event('focus');
    assert.equal(h.get('conflict').hidden, false);
    assert.equal(h.timers.size, 0);
    await h.click('reload');
    assert.equal(h.timers.size, 1);
    await h.click('close');
    assert.equal(h.timers.size, 0);
});
