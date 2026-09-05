const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const source = fs.readFileSync(path.join(__dirname, '../recog/web/static/app.js'), 'utf8');
const markup = fs.readFileSync(path.join(__dirname, '../recog/web/static/index.html'), 'utf8');
const flush = () => new Promise(resolve => setImmediate(resolve));
const response = data => ({ ok: true, json: async () => data });
const meta = (version = 1, extra = {}) => ({
    version, status: 'completed', generated_at: '2026-09-05T10:00:00Z', segment_count: 4,
    provider: 'codex', model: 'gpt-5.6', reasoning_effort: 'high', ...extra,
});
const result = (content = 'Saved answer', extra = {}) => ({ meta: meta(1, extra), content });
function deferred() {
    let resolve, reject;
    const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
    return { promise, resolve, reject };
}

function harness({ increments = { summary: [meta()] }, fetch: fetchOverride } = {}) {
    const elements = new Map(), queries = new Map(), streams = [], requests = [], timers = new Map(), copied = [];
    let timerId = 0;
    function element(id = '') {
        let html = '';
        const classes = new Set();
        const el = {
            id, textContent: '', style: {}, hidden: false, dataset: {}, value: '', disabled: false,
            classList: { toggle(name, active) { active ? classes.add(name) : classes.delete(name); }, contains: name => classes.has(name) },
            setAttribute() {}, addEventListener() {}, appendChild() {},
            querySelector(selector) { return get(id + ' ' + selector); },
            get innerHTML() { return html || this.textContent.replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;').replaceAll('"', '&quot;'); },
            set innerHTML(value) {
                html = value;
                for (const [, childId] of value.matchAll(/id="([^"]+)"/g)) elements.set(childId, element(childId));
                const buttons = [...value.matchAll(/<button ([^>]+)>/g)].map(([, attrs]) => {
                    const button = element();
                    for (const [, key, val] of attrs.matchAll(/data-([\w-]+)="([^"]+)"/g)) {
                        button.dataset[key.replace(/-([a-z])/g, (_, letter) => letter.toUpperCase())] = val;
                    }
                    button.classList.toggle('active', /class="[^"]*\bactive\b/.test(attrs));
                    return button;
                });
                if (id === 'ai-saved-view .modal-tabs') queries.set('#ai-saved-view .modal-tabs .tab-btn', buttons);
                if (id === 'ai-provider-tabs') queries.set('#ai-provider-tabs .tab-btn', buttons);
                if (id === 'version-tabs') queries.set('#version-tabs .version-tab-btn', buttons);
                if (id === 'ai-new-content') queries.set('#ai-new-content .ai-tab-content',
                    [...value.matchAll(/id="(ai-response-[^"]+)" class="([^"]+)"/g)].map(([, childId, className]) => {
                        const child = get(childId);
                        child.classList.toggle('active', className.includes('active'));
                        return child;
                    }));
            },
        };
        return el;
    }
    function get(id) { if (!elements.has(id)) elements.set(id, element(id)); return elements.get(id); }
    queries.set('#ai-modal [data-ai-mode]', ['saved', 'new'].map(mode => {
        const button = element(); button.dataset.aiMode = mode; return button;
    }));
    const context = vm.createContext({
        console, URLSearchParams, setTimeout, clearTimeout,
        setInterval(callback) { const id = ++timerId; timers.set(id, callback); return id; },
        clearInterval(id) { timers.delete(id); },
        fetch: async (url, options) => {
            requests.push({ url, options });
            if (fetchOverride) { const answer = fetchOverride(url, options); if (answer !== undefined) return answer; }
            if (url === '/api/prompts') return response([{ id: 'summary', name: 'Summary' }, { id: 'my-increment', name: 'My increment' }]);
            if (url.endsWith('/increments')) return response(increments);
            if (options?.method === 'POST') return response({ status: 'generating' });
            return response(result());
        },
        EventSource: class {
            constructor(url) { this.url = url; this.closed = false; streams.push(this); }
            close() { this.closed = true; }
            emit(data) { this.onmessage({ data: JSON.stringify(data) }); }
        },
        navigator: { clipboard: { writeText(text) { copied.push(text); return Promise.resolve(); } } },
        localStorage: { getItem: () => null, setItem() {} },
        window: { location: { hash: '#/meeting/a' } },
        document: {
            addEventListener() {}, createElement: () => element(), getElementById: get,
            querySelectorAll(selector) { assert.ok(queries.has(selector), `Unexpected selector: ${selector}`); return queries.get(selector); },
        },
    });
    vm.runInContext(source + '\nthis.meeting = views.meeting; this.snapshot = () => ({ session: aiModalSession, saved: _incrementState, responses: aiResponses, active: activeAiTab });', context);
    context.meeting.id = 'a';
    context.meeting.meta = { participants: {
        local: { name: 'Me', is_local: true }, remote: { name: 'Alex' }, __system__: { name: 'System' }, duplicate: { name: 'Alex' },
    } };
    return { context, get, queries, streams, requests, timers, copied,
        async open(id = 'a') { context.meeting.id = id; await context.analyzeIncrement(); await flush(); },
        snapshot: context.snapshot,
    };
}

test('saved history - open then toggle modes - keeps saved content and never starts generation', async () => {
    const h = harness(); await h.open();
    assert.equal(h.snapshot().session.mode, 'saved');
    assert.match(h.get('increment-content').innerHTML, /Codex · gpt-5.6 · high/);
    const savedHtml = h.get('increment-content').innerHTML;
    const count = h.requests.length;
    h.context.switchAiMode('new'); h.context.switchAiMode('saved');
    assert.equal(h.get('increment-content').innerHTML, savedHtml);
    assert.equal(h.requests.length, count);
    assert.equal(h.streams.length, 0);
    assert.equal(h.requests.filter(r => r.options?.method === 'POST').length, 0);
    assert.match(markup, /Сохранённые результаты/); assert.match(markup, /Новый анализ/);
});

test('empty history - open - selects idle new analysis with explicit launch', async () => {
    const h = harness({ increments: {} }); await h.open();
    assert.equal(h.snapshot().session.mode, 'new');
    assert.equal(h.streams.length, 0);
    assert.match(h.get('ai-new-content').innerHTML, /Запустить анализ/);
    assert.equal(h.get('ai-run-button').disabled, false);
    assert.equal(h.get('ai-provider-tabs').innerHTML, '');
    assert.match(markup, /не добавляются в сохранённые/);
});

for (const endpoint of ['/api/prompts', '/api/meetings/a/increments']) {
    test(`history endpoint ${endpoint} fails - open and retry - shows recoverable history without generation`, async () => {
        let failing = true;
        const h = harness({ fetch: url => url === endpoint && failing ? { ok: false } : undefined });
        await h.open();
        assert.equal(h.snapshot().session.mode, 'saved');
        assert.equal(h.snapshot().session.historyStatus, 'error');
        assert.match(h.get('ai-saved-view .modal-body').innerHTML, /Повторить загрузку/);
        assert.equal(h.streams.length, 0);
        failing = false; await h.context.loadAiHistory(); await flush();
        assert.match(h.get('increment-content').innerHTML, /Saved answer/);
    });
}

test('one installed provider - start frames and mode switches - only actual provider appears and output survives', async () => {
    const h = harness(); await h.open(); h.context.switchAiMode('new'); h.context.generateIncrementSSE();
    assert.equal(h.get('ai-provider-tabs').innerHTML, '');
    const stream = h.streams[0];
    stream.emit({ type: 'start', provider: 'codex', name: 'Codex (gpt-5.6 · high)' });
    stream.emit({ type: 'chunk', provider: 'codex', text: 'Partial answer' });
    assert.doesNotMatch(h.get('ai-provider-tabs').innerHTML, /claude/i);
    assert.match(h.get('ai-new-content').innerHTML, /increment-model-badge/);
    h.context.switchAiMode('saved');
    assert.equal(stream.closed, false);
    assert.match(h.get('ai-run-notice').textContent, /продолжается/);
    stream.emit({ type: 'done', provider: 'codex', text: 'Final answer' });
    stream.emit({ type: 'complete' });
    assert.equal(stream.closed, true);
    assert.match(h.get('increment-content').innerHTML, /Saved answer/);
    h.context.switchAiMode('new');
    assert.match(h.get('ai-new-content').innerHTML, /Final answer/);
    assert.equal(h.streams.length, 1);
    assert.equal(h.get('ai-run-button').disabled, false);
    h.context.copyAiResponse(); await flush(); assert.equal(h.copied.at(-1), 'Final answer');
    h.context.switchAiMode('saved'); h.context.copyAiResponse(); await flush(); assert.equal(h.copied.at(-1), 'Saved answer');
});

test('provider tabs and participant prompt - switch providers - leaves saved prompt and version filters scoped', async () => {
    const h = harness({ increments: { 'my-increment': [meta(), meta(2, { participant_id: 'remote' }), meta(3)] } });
    await h.open();
    assert.equal(h.snapshot().saved.currentVersion, 3);
    h.context.switchIncrementParticipant('remote'); await flush();
    assert.equal(h.snapshot().saved.currentVersion, 2);
    assert.match(h.get('version-tabs').innerHTML, /v2/); assert.doesNotMatch(h.get('version-tabs').innerHTML, /v3/);
    assert.equal(h.snapshot().saved.participants.length, 2);
    h.context.switchAiMode('new'); h.context.generateIncrementSSE();
    h.streams[0].emit({ type: 'start', provider: 'codex', name: 'Codex' });
    h.streams[0].emit({ type: 'start', provider: 'claude', name: 'Claude' });
    const prompt = h.queries.get('#ai-saved-view .modal-tabs .tab-btn')[0];
    assert.equal(prompt.classList.contains('active'), true);
    h.context.switchAiTab('claude');
    assert.equal(prompt.classList.contains('active'), true);
    h.context.switchAiMode('saved');
    assert.equal(h.snapshot().saved.currentParticipantId, 'remote');
    assert.equal(h.snapshot().saved.currentVersion, 2);
});

test('participant without saved versions - open and select participant - never generates implicitly', async () => {
    const h = harness({ increments: { 'my-increment': [meta(2, { participant_id: 'remote' })] } });
    await h.open();
    assert.match(h.get('increment-content').innerHTML, /Создать сохранённый результат/);
    assert.equal(h.requests.some(r => r.options?.method === 'POST'), false);
    await h.context.generateParticipantIncrement();
    assert.equal(h.requests.filter(r => r.options?.method === 'POST').length, 1);
});

test('in-flight history - close or open another meeting - stale success cannot reopen or replace results', async () => {
    const pending = deferred();
    const h = harness({ fetch: url => url === '/api/meetings/a/increments' ? pending.promise : undefined });
    const first = h.context.analyzeIncrement();
    h.context.closeAiModal(); await h.open('b');
    pending.resolve(response({ stale: [meta()] })); await first; await flush();
    assert.equal(h.snapshot().session.meetingId, 'b');
    assert.equal(h.snapshot().saved.currentPromptId, 'summary');
    assert.doesNotMatch(h.get('ai-saved-view .modal-tabs').innerHTML, /stale/);
});

test('in-flight content - mode switch then old failure - cannot replace current content or restart polling', async () => {
    const pending = deferred(); let first = true;
    const h = harness({ fetch: url => url.endsWith('/summary/1') && first ? (first = false, pending.promise) : undefined });
    await h.open(); h.context.switchAiMode('new'); h.context.switchAiMode('saved'); await flush();
    pending.reject(Error('stale failure')); await flush();
    assert.match(h.get('increment-content').innerHTML, /Saved answer/);
    assert.doesNotMatch(h.get('increment-content').innerHTML, /stale failure/);
    assert.equal(h.timers.size, 0);
});

test('in-flight content - close and reopen same meeting - stale generating callback cannot schedule polling', async () => {
    const pending = deferred(); let first = true;
    const h = harness({ fetch: url => url.endsWith('/summary/1') && first ? (first = false, pending.promise) : undefined });
    await h.open(); h.context.closeAiModal(); await h.open();
    pending.resolve(response(result('stale', { status: 'generating' }))); await flush();
    assert.match(h.get('increment-content').innerHTML, /Saved answer/);
    assert.equal(h.timers.size, 0);
});

test('closed or superseded stream - late callback - cannot alter new session or close its stream', async () => {
    const h = harness(); await h.open(); h.context.generateIncrementSSE();
    const old = h.streams[0]; await h.open('b'); h.context.generateIncrementSSE();
    const current = h.streams[1];
    assert.equal(old.closed, true);
    old.emit({ type: 'start', provider: 'old', name: 'Old' }); old.onerror(); old.emit({ type: 'complete' });
    assert.equal(current.closed, false);
    assert.equal(h.get('ai-provider-tabs').innerHTML, '');
    current.emit({ type: 'start', provider: 'codex', name: 'Codex' });
    current.emit({ type: 'chunk', provider: 'codex', text: 'Retained partial' }); current.onerror();
    assert.match(h.get('ai-new-content').innerHTML, /Retained partial/);
    assert.match(h.get('ai-new-content').innerHTML, /Соединение прервано/);
    assert.equal(h.get('ai-run-button').disabled, false);
    h.context.generateIncrementSSE();
    current.emit({ type: 'complete' });
    assert.equal(h.streams[2].closed, false);
});

test('saved regeneration - participant selection - keeps force and participant scoped to saved endpoint', async () => {
    const h = harness({ increments: { 'my-increment': [meta(), meta(2, { participant_id: 'remote' })] } });
    await h.open(); h.context.switchIncrementParticipant('remote'); await flush();
    await h.context.regeneratePrompt(); await flush();
    const post = h.requests.find(r => r.options?.method === 'POST');
    assert.equal(post.url, '/api/meetings/a/increments/my-increment/generate?force=true&participant_id=remote');
    assert.equal(h.snapshot().saved.currentParticipantId, 'remote');
    assert.equal(h.streams.length, 0);
});

test('server rejects analysis before provider start - error then closed connection - preserves reason and allows retry', async () => {
    const h = harness({ increments: {} }); await h.open(); h.context.generateIncrementSSE();
    const stream = h.streams[0];
    stream.emit({ type: 'error', message: 'No transcript segments available' });
    assert.equal(stream.closed, true);
    stream.onerror();
    assert.match(h.get('ai-new-content').innerHTML, /No transcript segments available/);
    assert.doesNotMatch(h.get('ai-new-content').innerHTML, /Соединение прервано/);
    assert.equal(h.get('ai-run-button').disabled, false);
});

test('no installed providers - stream completes without starts - exposes empty provider error and retry', async () => {
    const h = harness({ increments: {} }); await h.open(); h.context.generateIncrementSSE();
    h.streams[0].emit({ type: 'complete' });
    assert.match(h.get('ai-new-content').innerHTML, /Нет доступных провайдеров/);
    assert.equal(h.get('ai-run-button').disabled, false);
});

test('history loads while new mode selected - late saved result - keeps user mode and does not launch', async () => {
    const pending = deferred();
    const h = harness({ fetch: url => url.endsWith('/increments') ? pending.promise : undefined });
    const opening = h.context.analyzeIncrement(); h.context.switchAiMode('new');
    pending.resolve(response({ summary: [meta()] })); await opening; await flush();
    assert.equal(h.snapshot().session.mode, 'new');
    assert.equal(h.streams.length, 0);
    h.context.switchAiMode('saved'); await flush();
    assert.match(h.get('increment-content').innerHTML, /Saved answer/);
});

test('saved regeneration pending - switch New then Saved before completion - shows new version immediately', async () => {
    const pending = deferred();
    const increments = { summary: [meta()] };
    const h = harness({ increments, fetch: (_url, options) => options?.method === 'POST' ? pending.promise : undefined });
    await h.open();
    const generating = h.context.regeneratePrompt();
    h.context.switchAiMode('new'); h.context.switchAiMode('saved'); await flush();
    increments.summary.push(meta(2));
    pending.resolve(response({ status: 'generating' })); await generating; await flush();
    assert.equal(h.snapshot().saved.currentVersion, 2);
    assert.match(h.get('version-tabs').innerHTML, /v2/);
    assert.equal(h.snapshot().session.mode, 'saved');
});

for (const selection of ['version', 'prompt', 'participant']) {
    test(`saved regeneration pending - deliberately change ${selection} - completion keeps selected saved result`, async () => {
        const pending = deferred();
        const increments = { 'my-increment': [meta(), meta(2), meta(3, { participant_id: 'remote' })], summary: [meta()] };
        const h = harness({ increments, fetch: (_url, options) => options?.method === 'POST' ? pending.promise : undefined });
        await h.open();
        const generating = h.context.regeneratePrompt();
        if (selection === 'version') h.context.switchIncrementVersion('a', 'my-increment', 1);
        if (selection === 'prompt') h.context.switchPromptTab('a', 'summary');
        if (selection === 'participant') h.context.switchIncrementParticipant('remote');
        await flush();
        const chosen = { prompt: h.snapshot().saved.currentPromptId, version: h.snapshot().saved.currentVersion, participant: h.snapshot().saved.currentParticipantId };
        increments['my-increment'].push(meta(4));
        pending.resolve(response({ status: 'generating' })); await generating; await flush();
        h.context.switchAiMode('new'); h.context.switchAiMode('saved'); await flush();
        assert.deepEqual({ prompt: h.snapshot().saved.currentPromptId, version: h.snapshot().saved.currentVersion, participant: h.snapshot().saved.currentParticipantId }, chosen);
    });
}

test('participant generation pending - fail while New is visible then return Saved - shows error and allows retry', async () => {
    const pending = deferred(); let attempts = 0;
    const h = harness({ increments: { 'my-increment': [meta(2, { participant_id: 'remote' })] },
        fetch: (_url, options) => options?.method === 'POST' && attempts++ === 0 ? pending.promise : undefined });
    await h.open();
    const generating = h.context.generateParticipantIncrement();
    h.context.switchAiMode('new');
    pending.reject(Error('Provider unavailable')); await generating;
    h.context.switchAiMode('saved');
    assert.match(h.get('increment-content').innerHTML, /Provider unavailable/);
    assert.match(h.get('increment-content').innerHTML, /Создать сохранённый результат/);
    assert.doesNotMatch(h.get('increment-content').innerHTML, /Generating increment/);
    assert.equal(h.snapshot().saved.pendingGenerations.size, 0);
    await h.context.generateParticipantIncrement();
    assert.equal(h.requests.filter(r => r.options?.method === 'POST').length, 2);
});

test('participant generation pending - return Saved before completion - keeps pending state and suppresses duplicate requests', async () => {
    const pending = deferred();
    const h = harness({ increments: { 'my-increment': [meta(2, { participant_id: 'remote' })] },
        fetch: (_url, options) => options?.method === 'POST' ? pending.promise : undefined });
    await h.open(); const generating = h.context.generateParticipantIncrement();
    h.context.switchAiMode('new'); h.context.switchAiMode('saved');
    assert.match(h.get('increment-content').innerHTML, /Generating increment/);
    await h.context.generateParticipantIncrement();
    assert.equal(h.requests.filter(r => r.options?.method === 'POST').length, 1);
    pending.reject(Error('Provider unavailable')); await generating;
    assert.match(h.get('increment-content').innerHTML, /Provider unavailable/);
});

test('participant generation succeeds - choose another participant older version then toggle modes - keeps deliberate selection', async () => {
    const increments = { 'my-increment': [meta(1, { participant_id: 'remote' }), meta(2, { participant_id: 'remote' })] };
    const h = harness({ increments, fetch: (_url, options) => {
        if (options?.method !== 'POST') return undefined;
        increments['my-increment'].push(meta(3));
        return response({ status: 'generating' });
    } });
    await h.open(); await h.context.generateParticipantIncrement(); await flush();
    assert.equal(h.snapshot().saved.currentVersion, 3);
    assert.equal(h.snapshot().saved.needsRefresh, false);
    h.context.switchIncrementParticipant('remote');
    h.context.switchIncrementVersion('a', 'my-increment', 1); await flush();
    h.context.switchAiMode('new'); h.context.switchAiMode('saved'); await flush();
    assert.equal(h.snapshot().saved.currentParticipantId, 'remote');
    assert.equal(h.snapshot().saved.currentVersion, 1);
    assert.match(markup, /Мой инкремент к следующей встрече/);
});
