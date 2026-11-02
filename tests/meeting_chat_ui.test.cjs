const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const staticDir = path.join(__dirname, '../recog/web/static');
const chatPath = path.join(staticDir, 'meeting-chat.js');
const appPath = path.join(staticDir, 'app.js');
const chatSource = fs.readFileSync(chatPath, 'utf8');
const appSource = fs.readFileSync(appPath, 'utf8');
const markup = fs.readFileSync(path.join(staticDir, 'index.html'), 'utf8');
const flush = () => new Promise(resolve => setImmediate(resolve));
const ok = data => ({ ok: true, json: async () => data });
const turn = (question, answer, extra = {}) => ({
    asked_at: '2026-11-02T10:00:00+00:00', question, answer, provider: 'Claude (opus)',
    settings_revision: 'defaults', transcript_truncated: false, ...extra,
});
function deferred() {
    let resolve;
    const promise = new Promise(yes => { resolve = yes; });
    return { promise, resolve };
}

function element(id = '') {
    let html = '';
    const classes = new Set();
    return {
        id, value: '', hidden: false, disabled: false, textContent: '', style: {}, dataset: {},
        listeners: {}, attributes: {}, scrolled: null, focused: false, form: null,
        classList: {
            add(name) { classes.add(name); }, remove(name) { classes.delete(name); },
            toggle(name, active) { active ? classes.add(name) : classes.delete(name); },
            contains: name => classes.has(name),
        },
        get innerHTML() { return html; },
        set innerHTML(value) { html = value; },
        addEventListener(name, callback) { this.listeners[name] = callback; },
        setAttribute(name, value) { this.attributes[name] = value; },
        removeAttribute(name) { delete this.attributes[name]; },
        scrollIntoView(options) { this.scrolled = options; },
        focus() { this.focused = true; },
        requestSubmit() { this.listeners.submit({ preventDefault() {} }); },
        querySelectorAll() { return []; },
    };
}

function harness(fetchFn, { hash = '#/meeting/m1' } = {}) {
    const elements = new Map();
    for (const [tag, id] of markup.matchAll(/<[^>]*\bid="([^"]+)"[^>]*>/g)) {
        elements.set(id, Object.assign(element(id), { hidden: /\shidden[\s>]/.test(tag) }));
    }
    const segmentElements = new Map();
    const windowListeners = {}, documentListeners = [];
    const requests = [];
    const document = {
        addEventListener(name, callback) { documentListeners.push({ name, callback }); },
        getElementById(id) { assert.ok(elements.has(id), `Missing markup: ${id}`); return elements.get(id); },
        createElement() { return { textContent: '', get innerHTML() { return this.textContent.replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;'); } }; },
        querySelectorAll(selector) {
            assert.equal(selector, '#transcript .transcript-segment');
            const found = [...elements.get('transcript').innerHTML.matchAll(/data-segment-indices="([^"]*)"/g)];
            return found.map(([, indices]) => {
                if (!segmentElements.has(indices)) {
                    const el = element();
                    el.dataset.segmentIndices = indices;
                    segmentElements.set(indices, el);
                }
                return segmentElements.get(indices);
            });
        },
    };
    const window = {
        location: { hash },
        addEventListener(name, callback) { windowListeners[name] = callback; },
    };
    const context = vm.createContext({
        console, setTimeout, clearTimeout, window, document,
        localStorage: { getItem: () => null, setItem() {} },
        sessionStorage: { getItem: () => null, setItem() {}, removeItem() {} },
        fetch: (url, options = {}) => {
            requests.push({ url, method: options.method || 'GET', body: options.body && JSON.parse(options.body) });
            return fetchFn(url, options);
        },
    });
    vm.runInContext(chatSource, context, { filename: chatPath });
    vm.runInContext(appSource, context, { filename: appPath });
    vm.runInContext('this.views = views;', context);
    documentListeners[0].callback();
    const el = id => elements.get(id);
    return {
        el, requests, views: context.views, segmentElements,
        log: () => el('meeting-chat-log').innerHTML,
        open: async () => { el('meeting-chat-btn').listeners.click(); await flush(); },
        navigate: async next => { window.location.hash = next; windowListeners.hashchange(); await flush(); },
        submit: async text => {
            el('meeting-chat-input').value = text;
            el('meeting-chat-form').listeners.submit({ preventDefault() {} });
            await flush();
        },
        clickCitation: ts => el('meeting-chat-log').listeners.click({
            target: { closest: selector => (selector === '.chat-cite' ? { dataset: { ts } } : null) },
        }),
    };
}

test('closed panel - opening - shows the saved dialog of this meeting', async () => {
    const h = harness(async () => ok([turn('Что решили?', 'Ждать пятницу [11:30:14].')]));
    assert.equal(h.el('meeting-chat').hidden, true, 'closed until asked for');
    assert.equal(h.requests.length, 0, 'nothing fetched while closed');

    await h.open();

    assert.equal(h.el('meeting-chat').hidden, false);
    assert.equal(h.el('meeting-chat-btn').attributes['aria-expanded'], 'true');
    assert.equal(h.el('view-meeting').classList.contains('chat-open'), true);
    assert.deepEqual(h.requests[0], { url: '/api/meetings/m1/chat', method: 'GET', body: undefined });
    assert.match(h.log(), /Что решили\?/);
    assert.match(h.log(), /Ждать пятницу/);
    assert.equal(h.el('meeting-chat-input').focused, true);
});

test('meeting without a dialog - opening - explains what can be asked', async () => {
    const h = harness(async () => ok([]));
    await h.open();
    assert.match(h.log(), /Спросите/);
    assert.equal(h.el('meeting-chat-clear').hidden, true);
});

test('question - while the model answers - stays visible, then the answer replaces the wait', async () => {
    const answer = deferred();
    const h = harness(async (url, options) => (options.method === 'POST' ? answer.promise : ok([])));
    await h.open();

    await h.submit('  Что сказал Пётр?  ');

    assert.deepEqual(h.requests[1], { url: '/api/meetings/m1/chat', method: 'POST', body: { question: 'Что сказал Пётр?' } });
    assert.match(h.log(), /Что сказал Пётр\?/);
    assert.match(h.log(), /meeting-chat-waiting/);
    assert.equal(h.el('meeting-chat-input').value, '');
    assert.equal(h.el('meeting-chat-send').disabled, true);
    assert.equal(h.el('meeting-chat-clear').hidden, true, 'no clearing under a running question');

    answer.resolve(ok(turn('Что сказал Пётр?', 'За синюю кнопку [11:30:14], [11:31:00].')));
    await flush();

    assert.doesNotMatch(h.log(), /meeting-chat-waiting/);
    assert.match(h.log(), /За синюю кнопку/);
    assert.match(h.log(), /Claude \(opus\)/);
    assert.equal(h.el('meeting-chat-send').disabled, false);
    assert.equal(h.el('meeting-chat-clear').hidden, false);
});

test('second question - while the first runs - is not sent', async () => {
    const h = harness(async (url, options) => (options.method === 'POST' ? deferred().promise : ok([])));
    await h.open();
    await h.submit('Первый');
    await h.submit('Второй');
    assert.equal(h.requests.filter(request => request.method === 'POST').length, 1);
    assert.equal(h.el('meeting-chat-input').value, 'Второй', 'the typed text is kept');
});

test('blank question - submitting - sends nothing', async () => {
    const h = harness(async () => ok([]));
    await h.open();
    await h.submit('  \n ');
    assert.equal(h.requests.filter(request => request.method === 'POST').length, 0);
});

test('markup in a question or answer - rendering - stays text', async () => {
    const h = harness(async () => ok([turn('<img src=x onerror=alert(1)>', '<script>alert(2)</script>')]));
    await h.open();
    assert.doesNotMatch(h.log(), /<img|<script/);
    assert.match(h.log(), /&lt;img/);
});

test('failed model call - answering - shows why and returns the question for a retry', async () => {
    const retry = deferred();
    let attempts = 0;
    const h = harness(async (url, options) => {
        if (options.method !== 'POST') return ok([]);
        attempts++;
        return attempts === 1
            ? { ok: false, status: 502, json: async () => ({ detail: 'Claude CLI error: limit' }) }
            : retry.promise;
    });
    await h.open();

    await h.submit('Что решили?');

    assert.equal(h.el('meeting-chat-error').hidden, false);
    assert.match(h.el('meeting-chat-error').textContent, /limit/);
    assert.equal(h.el('meeting-chat-input').value, 'Что решили?');
    assert.doesNotMatch(h.log(), /Что решили\?/);
    assert.equal(h.el('meeting-chat-send').disabled, false);

    await h.submit('Что решили?');
    assert.equal(attempts, 2);
    assert.equal(h.el('meeting-chat-error').hidden, true, 'a new attempt clears the old error');
});

test('unreachable server - asking - reports it instead of hanging', async () => {
    const h = harness(async (url, options) => {
        if (options.method === 'POST') throw new TypeError('Failed to fetch');
        return ok([]);
    });
    await h.open();
    await h.submit('Вопрос');
    assert.match(h.el('meeting-chat-error').textContent, /Failed to fetch/);
    assert.equal(h.el('meeting-chat-input').value, 'Вопрос');
});

test('moving to another meeting - during an answer - keeps each dialog in its own meeting', async () => {
    const answer = deferred();
    const dialogs = { m1: [], m2: [turn('Вопрос второй встречи', 'Её ответ')] };
    const h = harness(async (url, options) => {
        if (options.method === 'POST') return answer.promise;
        return ok(dialogs[url.split('/')[3]]);
    });
    await h.open();
    await h.submit('Вопрос первой встречи');

    await h.navigate('#/meeting/m2');
    assert.doesNotMatch(h.log(), /Вопрос первой встречи/);
    assert.match(h.log(), /Вопрос второй встречи/);
    assert.equal(h.el('meeting-chat-send').disabled, false, 'another meeting can be asked meanwhile');

    answer.resolve(ok(turn('Вопрос первой встречи', 'Ответ первой')));
    await flush();
    assert.doesNotMatch(h.log(), /Ответ первой/);

    dialogs.m1 = [turn('Вопрос первой встречи', 'Ответ первой')];
    await h.navigate('#/meeting/m1');
    assert.match(h.log(), /Ответ первой/);
});

test('returning to a meeting - while its answer runs - shows the question waiting', async () => {
    const answer = deferred();
    const h = harness(async (url, options) => (options.method === 'POST' ? answer.promise : ok([])));
    await h.open();
    await h.submit('Вопрос');
    await h.navigate('#/meeting/m2');
    await h.navigate('#/meeting/m1');
    assert.match(h.log(), /Вопрос/);
    assert.match(h.log(), /meeting-chat-waiting/);
    assert.equal(h.el('meeting-chat-send').disabled, true);
});

test('failure in a meeting left meanwhile - returning - offers its question again', async () => {
    const answer = deferred();
    const h = harness(async (url, options) => (options.method === 'POST' ? answer.promise : ok([])));
    await h.open();
    await h.submit('Вопрос');
    await h.navigate('#/meeting/m2');
    answer.resolve({ ok: false, status: 502, json: async () => ({ detail: 'сбой' }) });
    await flush();
    assert.equal(h.el('meeting-chat-error').hidden, true);
    assert.equal(h.el('meeting-chat-input').value, '');

    await h.navigate('#/meeting/m1');

    assert.match(h.el('meeting-chat-error').textContent, /сбой/);
    assert.equal(h.el('meeting-chat-input').value, 'Вопрос');
});

test('answer of an older load - arriving after moving on - is ignored', async () => {
    const first = deferred();
    const h = harness(async url => (url.includes('/m1/') ? first.promise : ok([turn('Вторая', 'Ответ')])));
    await h.open();
    await h.navigate('#/meeting/m2');
    first.resolve(ok([turn('Первая', 'Старое')]));
    await flush();
    assert.doesNotMatch(h.log(), /Старое/);
    assert.match(h.log(), /Вторая/);
});

test('dialog that cannot load - opening - says so', async () => {
    const h = harness(async () => ({ ok: false, status: 500, json: async () => ({ detail: 'нет доступа' }) }));
    await h.open();
    assert.match(h.el('meeting-chat-error').textContent, /нет доступа/);
});

test('leaving for a page without a meeting - panel open - fetches nothing', async () => {
    const h = harness(async () => ok([]));
    await h.open();
    const before = h.requests.length;
    await h.navigate('#/live');
    assert.equal(h.requests.length, before);
});

test('citation in an answer - rendering - becomes a button with its time', async () => {
    const h = harness(async () => ok([turn('В', 'Сказал «нет» [11:30:14] и в 11:31:00.')]));
    await h.open();
    const times = [...h.log().matchAll(/<button[^>]*class="chat-cite"[^>]*data-ts="([\d:]+)"[^>]*>/g)].map(match => match[1]);
    assert.deepEqual(times, ['11:30:14', '11:31:00']);
    assert.doesNotMatch(h.log(), /\[<button/);
});

test('citation - clicking - scrolls the transcript to the nearest replica and marks it', async () => {
    const h = harness(async () => ok([]));
    const segments = [
        { ts_display: '11:30:00', speaker: 'A', speaker_id: 'a', text: 'раз' },
        { ts_display: '11:30:10', speaker: 'Система', event_type: 'session_pause', text: 'пауза' },
        { ts_display: '11:35:00', speaker: 'B', speaker_id: 'b', text: 'два' },
        { ts_display: '11:40:00', speaker: 'A', speaker_id: 'a', text: 'три' },
    ];
    h.views.meeting.segments = segments;
    h.views.meeting.renderTranscript(segments, h.el('transcript'));
    await h.open();

    h.clickCitation('11:34:50');

    const marked = h.segmentElements.get('2');
    assert.equal(marked.classList.contains('segment-cited'), true);
    assert.equal(marked.scrolled.block, 'center');

    h.clickCitation('11:30:12');
    assert.equal(marked.classList.contains('segment-cited'), false, 'only the last citation is marked');
    assert.equal(h.segmentElements.get('0').classList.contains('segment-cited'), true);
});

test('citation - with a transcript search active - clears the search so the replica is there', async () => {
    const h = harness(async () => ok([]));
    const segments = [
        { ts_display: '11:30:00', speaker: 'A', speaker_id: 'a', text: 'раз' },
        { ts_display: '11:35:00', speaker: 'B', speaker_id: 'b', text: 'два' },
    ];
    h.views.meeting.segments = segments;
    h.views.meeting.renderTranscript(segments.slice(1), h.el('transcript'), 'два');
    h.el('transcript-search').value = 'два';
    await h.open();

    h.clickCitation('11:30:00');

    assert.equal(h.el('transcript-search').value, '');
    assert.equal(h.segmentElements.get('0').classList.contains('segment-cited'), true);
});

test('click beside a citation - in the log - does nothing', async () => {
    const h = harness(async () => ok([]));
    await h.open();
    h.el('meeting-chat-log').listeners.click({ target: { closest: () => null } });
    assert.equal(h.segmentElements.size, 0);
});

test('Enter - in the question field - sends, except Shift+Enter and IME composition', async () => {
    const h = harness(async () => ok(turn('В', 'О')));
    await h.open();
    const input = h.el('meeting-chat-input');
    let sent = 0;
    h.el('meeting-chat-form').requestSubmit = () => { sent++; };
    const press = extra => {
        let prevented = false;
        input.listeners.keydown({ key: 'Enter', preventDefault() { prevented = true; }, ...extra });
        return prevented;
    };
    assert.equal(press({ shiftKey: true }), false);
    assert.equal(sent, 0);
    assert.equal(press({ isComposing: true }), true);
    assert.equal(press({ keyCode: 229 }), true);
    assert.equal(sent, 0);
    assert.equal(press({}), true);
    assert.equal(sent, 1);
    input.listeners.keydown({ key: 'a', preventDefault() { assert.fail('other keys pass'); } });
});

test('new conversation - clicking - forgets the dialog on the server and on screen', async () => {
    const h = harness(async (url, options) => (options.method === 'DELETE' ? ok({ success: true }) : ok([turn('В', 'О')])));
    await h.open();

    h.el('meeting-chat-clear').listeners.click();
    await flush();

    assert.equal(h.requests.at(-1).method, 'DELETE');
    assert.doesNotMatch(h.log(), /chat-turn/);
    assert.equal(h.el('meeting-chat-clear').hidden, true);
});

test('new conversation - when the server refuses - keeps the dialog and says why', async () => {
    const h = harness(async (url, options) => (options.method === 'DELETE'
        ? { ok: false, status: 500, json: async () => ({ detail: 'занято' }) }
        : ok([turn('В', 'Ответ')])));
    await h.open();
    h.el('meeting-chat-clear').listeners.click();
    await flush();
    assert.match(h.log(), /Ответ/);
    assert.match(h.el('meeting-chat-error').textContent, /занято/);
});

test('cut transcript - rendering the turn - warns that the model saw only the end', async () => {
    const h = harness(async () => ok([turn('В', 'О', { transcript_truncated: true })]));
    await h.open();
    assert.match(h.log(), /только её конец/);
});

test('Escape - inside the panel - closes it and returns focus to the button', async () => {
    const h = harness(async () => ok([]));
    await h.open();

    h.el('meeting-chat').listeners.keydown({ key: 'Escape' });

    assert.equal(h.el('meeting-chat').hidden, true);
    assert.equal(h.el('view-meeting').classList.contains('chat-open'), false);
    assert.equal(h.el('meeting-chat-btn').attributes['aria-expanded'], 'false');
    assert.equal(h.el('meeting-chat-btn').focused, true);
    h.el('meeting-chat').listeners.keydown({ key: 'a' });
    assert.equal(h.el('meeting-chat').hidden, true);
});

test('close button and the toggle - clicking - close the panel', async () => {
    const h = harness(async () => ok([]));
    await h.open();
    h.el('meeting-chat-close').listeners.click();
    assert.equal(h.el('meeting-chat').hidden, true);
    await h.open();
    assert.equal(h.el('meeting-chat').hidden, false);
    await h.open();
    assert.equal(h.el('meeting-chat').hidden, true);
});

test('panel left open - coming to another meeting - loads its dialog', async () => {
    const h = harness(async () => ok([]), { hash: '#/' });
    await h.navigate('#/meeting/m7');
    assert.equal(h.requests.length, 0, 'closed: nothing to load');
    await h.open();
    assert.equal(h.requests[0].url, '/api/meetings/m7/chat');
});
