const { test } = require('node:test');
const assert = require('node:assert/strict');
const { loadPage, json } = require('./web_page.cjs');

function statusPage(answer, { honourAbort = true } = {}) {
    const pending = [];
    const fetch = (url, options) => {
        assert.equal(url, '/api/desktop/status');
        assert.equal(options.cache, 'no-store');
        return new Promise((resolve, reject) => {
            const call = { resolve, reject, signal: options.signal };
            const abort = () => {
                const error = new Error('aborted');
                error.name = 'AbortError';
                reject(error);
            };
            if (honourAbort) {
                if (options.signal.aborted) abort();
                options.signal.addEventListener('abort', abort);
            }
            pending.push(call);
            if (answer) Promise.resolve(answer(call)).then(resolve, reject);
        });
    };
    const page = loadPage({ hash: '#/', scripts: ['desktop-status.js'], fetch });
    const settings = page.$('profile-settings');
    return {
        ...page,
        pending,
        line: () => page.text(page.$('desktop-status')),
        toggle(open) {
            settings.open = open;
            settings.dispatchEvent({ type: 'toggle', bubbles: false });
        },
        visibility(hidden) {
            page.document.hidden = hidden;
            page.document.dispatchEvent({ type: 'visibilitychange' });
        },
        settle: async () => { for (let i = 0; i < 5; i++) await page.flush(); },
    };
}

test('settings closed - page loaded - status is not requested', () => {
    const p = statusPage(() => json({}));
    assert.equal(p.line(), 'KTalk Desktop: проверка…');
    assert.equal(p.requests.length, 0);
});

test('settings opened - attached with a version and a reason - all three are shown', async () => {
    const p = statusPage(() => json({ enabled: true, state: 'attached', app_version: '25.1', reason: 'inspector_port_busy' }));
    p.toggle(true);
    await p.settle();
    assert.equal(p.line(), 'KTalk Desktop: расширение подключено · 25.1 · порт инспектора занят');
});

test('settings opened - integration disabled - shown as off whatever the state', async () => {
    const p = statusPage(() => json({ enabled: false, state: 'attached' }));
    p.toggle(true);
    await p.settle();
    assert.equal(p.line(), 'KTalk Desktop: выключена');
});

test('settings opened - inspector left open - shown even with the integration off', async () => {
    const p = statusPage(() => json({ enabled: false, state: 'inspector_unclosed' }));
    p.toggle(true);
    await p.settle();
    assert.equal(p.line(), 'KTalk Desktop: закрытие инспектора не подтверждено');
});

test('settings opened - unknown state and reason - a connection error without the reason', async () => {
    const p = statusPage(() => json({ enabled: true, state: 'new_state', reason: 'new_reason' }));
    p.toggle(true);
    await p.settle();
    assert.equal(p.line(), 'KTalk Desktop: ошибка подключения');
});

test('settings opened - server answers an error or is unreachable - server unavailable', async () => {
    const failing = statusPage(() => json({}, { status: 500 }));
    failing.toggle(true);
    await failing.settle();
    assert.equal(failing.line(), 'KTalk Desktop: сервер недоступен');
    const offline = statusPage(() => Promise.reject(new Error('offline')));
    offline.toggle(true);
    await offline.settle();
    assert.equal(offline.line(), 'KTalk Desktop: сервер недоступен');
});

test('settings open - five seconds pass - the status is polled again', async () => {
    let state = 'waiting';
    const p = statusPage(() => json({ enabled: true, state }));
    p.toggle(true);
    await p.settle();
    assert.equal(p.line(), 'KTalk Desktop: ожидание приложения');
    state = 'attaching';
    p.timers.advance(5000);
    await p.settle();
    assert.equal(p.requests.length, 2);
    assert.equal(p.line(), 'KTalk Desktop: подключение расширения');
});

test('settings open - closed before the answer - the request is aborted and nothing more is polled', async () => {
    const p = statusPage();
    p.toggle(true);
    p.toggle(false);
    await p.settle();
    assert.equal(p.pending[0].signal.aborted, true);
    assert.equal(p.line(), 'KTalk Desktop: проверка…');
    p.timers.advance(10000);
    assert.equal(p.requests.length, 1);
});

test('settings open - an old answer arrives after reopening - only the new answer is shown', async () => {
    const p = statusPage(undefined, { honourAbort: false });
    p.toggle(true);
    await p.flush();
    p.toggle(true);
    await p.flush();
    p.pending[1].resolve(json({ enabled: true, state: 'error' }));
    await p.settle();
    p.pending[0].resolve(json({ enabled: true, state: 'attached' }));
    await p.settle();
    assert.equal(p.line(), 'KTalk Desktop: ошибка подключения');
    assert.deepEqual(p.timers.pending().map(timer => timer.in), [5000]);
});

test('settings open - tab hidden, then shown - polling stops and resumes', async () => {
    const p = statusPage(() => json({ enabled: true, state: 'attached' }));
    p.toggle(true);
    await p.settle();
    p.visibility(true);
    p.timers.advance(10000);
    assert.equal(p.requests.length, 1);
    p.visibility(false);
    await p.settle();
    assert.equal(p.requests.length, 2);
});

test('settings closed - tab shown again - nothing is requested', () => {
    const p = statusPage(() => json({}));
    p.visibility(false);
    assert.equal(p.requests.length, 0);
});

test('tab hidden - settings opened - nothing is requested until the tab is shown', async () => {
    const p = statusPage(() => json({ enabled: true, state: 'attached' }));
    p.document.hidden = true;
    p.toggle(true);
    assert.equal(p.requests.length, 0);
    p.visibility(false);
    await p.settle();
    assert.equal(p.line(), 'KTalk Desktop: расширение подключено');
});

test('request in flight - tab hidden before the answer - answer shown and no next poll', async () => {
    let release;
    const p = statusPage(() => new Promise(resolve => { release = () => resolve(json({ enabled: true, state: 'attached' })); }));
    p.toggle(true);
    await p.flush();
    p.document.hidden = true;
    release();
    await p.settle();
    assert.equal(p.line(), 'KTalk Desktop: расширение подключено');
    assert.deepEqual(p.timers.pending(), []);
});
