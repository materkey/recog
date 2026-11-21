const { test } = require('node:test');
const assert = require('node:assert/strict');
const { loadPage, json, makeEvent } = require('./web_page.cjs');

const localOrigin = 'http://localhost:9080';

function settings(allowedOrigin, postStatus = 202) {
    const page = loadPage({
        scripts: ['installation-readiness.js'],
        fetch: async (url, options) => options.method === 'POST'
            ? json(null, { status: postStatus })
            : { ...json([{ name: 'Микрофон', ready: true }]), headers: new Headers(
                allowedOrigin ? { 'recog-permissions-origin': allowedOrigin } : {},
            ) },
    });
    page.window.top = page.window.self;
    const panel = page.$('installation-readiness');
    const button = page.$('macos-permissions');
    return {
        ...page, button,
        async open() {
            panel.open = true;
            panel.dispatchEvent(makeEvent('toggle'));
            await page.flush();
        },
        click() { button.dispatchEvent(makeEvent('click', { isTrusted: true })); },
    };
}

test('local macOS settings - inspect then click - only the click sends one empty same-origin POST', async () => {
    const p = settings(localOrigin);
    assert.equal(p.button.hidden, true);
    assert.equal(p.requests.length, 0);
    await p.open();
    assert.equal(p.button.hidden, false);
    assert.equal(p.text(p.button), 'Разрешения macOS…');
    assert.equal(p.text(p.$('installation-checks')), 'Микрофон: готово');
    p.button.click(); // Programmatic activation.
    assert.deepEqual(p.requests.map(r => [r.method, r.url]), [['GET', '/api/readiness']]);
    p.click();
    p.click();
    await p.flush();
    assert.equal(p.requests.length, 2);
    const post = p.requests[1];
    assert.deepEqual([post.method, post.url, post.body, post.options.mode, post.options.redirect],
        ['POST', '/api/readiness/macos-permissions', undefined, 'same-origin', 'error']);
    assert.equal(p.button.disabled, false);
});

test('unavailable, proxied or framed settings - view readiness - the native action stays hidden', async () => {
    for (const [allowed, pageOrigin, framed] of [
        [null, localOrigin, false], [localOrigin, 'https://recog.example', false], [localOrigin, localOrigin, true],
    ]) {
        const p = settings(allowed);
        p.window.location.origin = pageOrigin;
        if (framed) p.window.top = {};
        await p.open();
        assert.equal(p.button.hidden, true);
        p.click();
        assert.deepEqual(p.requests.map(r => r.method), ['GET']);
    }
});

test('local click - window already open or unavailable - status explains the result', async () => {
    for (const [status, message] of [[409, 'Окно разрешений уже открыто.'], [503, 'Не удалось открыть окно разрешений.']]) {
        const p = settings(localOrigin, status);
        await p.open();
        p.click();
        await p.flush();
        assert.equal(p.text(p.$('macos-permissions-status')), message);
    }
});
