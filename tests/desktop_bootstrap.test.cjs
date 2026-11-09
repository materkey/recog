// recog/desktop/bootstrap.js as recog-desktop evaluates it in the KTalk desktop
// application's main process: the script's text, then a call of its top-level
// `attachDesktop` with the bundle directory, its digest, the runtime status
// file and the target (crates/recog-desktop/src/worker.rs, `bootstrap`). The
// main process is a vm context with fakes for Electron; each window's page is
// a context of its own, where `executeJavaScript` runs the page code.

const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const vm = require('node:vm');

const sourcePath = path.join(__dirname, '../recog/desktop/bootstrap.js');
const source = fs.readFileSync(sourcePath, 'utf8');

const DIGEST = 'sha256-of-bundle';
const TARGET = { pid: 4242, started: 1700000000.5, executable: '/opt/ktalk/ktalk' };
// The bundle's page script: it reports the outcome its page was set up with.
const INJECTED = 'window.__recogDesktopRuntime = window.outcome ? { state: window.outcome } : undefined;';

const flush = () => new Promise(resolve => setImmediate(resolve));
const plain = value => JSON.parse(JSON.stringify(value));

function session({ persistent = true, api = 'extensions', onLoad = () => {} } = {}) {
    const loads = [];
    const loadExtension = async directory => {
        loads.push(directory);
        onLoad();
        return { id: 'extension-id' };
    };
    const own = { isPersistent: () => persistent, loads };
    if (api === 'extensions') own.extensions = { loadExtension };
    if (api === 'session') own.loadExtension = loadExtension;
    return own;
}

// A web contents; without a session of its own it is in the default session.
function contents(url, { type = 'window', outcome = 'ready', session: own = null, location = url } = {}) {
    const page = vm.createContext({ outcome, URL });
    page.window = page;
    page.location = { href: location };
    const handlers = {};
    const executed = [];
    let destroyed = false;
    return {
        page,
        executed,
        session: own,
        destroy() { destroyed = true; },
        isDestroyed: () => destroyed,
        getURL: () => url,
        getType: () => type,
        on(event, handler) { (handlers[event] ||= []).push(handler); },
        async executeJavaScript(code, userGesture) {
            executed.push(userGesture);
            return vm.runInContext(code, page);
        },
        // The page finished loading a document.
        async domReady() {
            for (const handler of handlers['dom-ready'] || []) handler();
            await flush();
        },
    };
}

function application({ windows = [], defaultSession = session(), globals = null, statusPath = null, injected = INJECTED } = {}) {
    const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'recog-bootstrap-'));
    fs.writeFileSync(path.join(directory, 'injected.js'), injected);
    const listeners = {};
    const electron = {
        app: {
            getVersion: () => '3.14.1',
            whenReady: async () => {},
            on(event, listener) { (listeners[event] ||= []).push(listener); },
        },
        session: { defaultSession },
        webContents: { getAllWebContents: () => windows },
    };
    const inDefaultSession = window => { window.session ||= defaultSession; };
    windows.forEach(inDefaultSession);
    const required = [];
    const fakeRequire = name => {
        required.push(name);
        return name === 'electron' ? electron : require(name);
    };
    // Node's URL is a global of Electron's main process, not of a bare vm context.
    const context = vm.createContext({ URL, ...(globals ? globals(fakeRequire) : { require: fakeRequire, process: { pid: 77 } }) });
    vm.runInContext(source, context, { filename: sourcePath });
    const status = statusPath || path.join(directory, 'runtime.json');
    return {
        context,
        directory,
        listeners,
        required,
        status,
        attach: () => vm.runInContext(
            `attachDesktop(${[directory, DIGEST, status, TARGET].map(value => JSON.stringify(value)).join(',')})`,
            context,
        ),
        runtime: () => JSON.parse(fs.readFileSync(status, 'utf8')),
        record: () => context.__recogDesktopIntegration,
        // A window the application opens after the attachment.
        async open(window) {
            inDefaultSession(window);
            for (const listener of listeners['web-contents-created'] || []) listener({}, window);
            await window.domReady();
        },
    };
}

test('a KTalk window - attached - injects the bundle and answers the app version and digest', async () => {
    const window = contents('https://example.ktalk.ru/room');
    const app = application({ windows: [window] });

    const answer = await app.attach();

    assert.deepEqual(plain(answer), { app_version: '3.14.1', bundle: DIGEST });
    assert.equal(app.record().state, 'ready');
    assert.equal(window.page.__PCM_WORKLET_URL__, 'chrome-extension://extension-id/src/audio-worklet/downsampler.js');
    assert.equal(window.page.__recogDesktopRuntime.state, 'ready');
    assert.deepEqual(window.executed, [true]);
    assert.deepEqual(app.required, ['electron', 'node:fs']);
});

test('the default session - attached - loads the bundle directory once per session', async () => {
    const shared = session();
    const windows = [
        contents('https://ktalk.ru/a', { session: shared }),
        contents('https://ktalk.ru/b', { session: shared }),
    ];
    const app = application({ windows, defaultSession: shared });

    await app.attach();

    assert.deepEqual(shared.loads, [app.directory]);
    assert.deepEqual(windows.map(window => window.executed.length), [1, 1]);
});

test('an older Electron - session without extensions - loads through the session itself', async () => {
    const own = session({ api: 'session' });
    const app = application({ windows: [contents('https://ktalk.ru/', { session: own })], defaultSession: own });

    await app.attach();

    assert.deepEqual(own.loads, [app.directory]);
});

test('a session without an extension API - attached - fails and marks the integration failed', async () => {
    const app = application({ defaultSession: session({ api: 'none' }) });

    await assert.rejects(app.attach(), { message: 'Electron extension API unavailable' });

    assert.equal(app.record().state, 'failed');
});

test('a window in a non-persistent session - attached - fails without running page code', async () => {
    const window = contents('https://ktalk.ru/', { session: session({ persistent: false }) });
    const app = application({ windows: [window] });

    await assert.rejects(app.attach(), { message: 'Non-persistent Electron session' });

    assert.equal(app.record().state, 'failed');
    assert.deepEqual(window.executed, []);
});

test('windows that are not KTalk calls - attached - are left alone', async () => {
    const urls = [
        'http://ktalk.ru/',
        'https://ktalk.ru:8443/',
        'https://evilktalk.ru/',
        'https://ktalk.ru.example.org/',
        'not a url',
    ];
    const windows = urls.map(url => contents(url));
    const devtools = contents('https://ktalk.ru/', { type: 'webview' });
    const app = application({ windows: [...windows, devtools] });

    await app.attach();

    assert.deepEqual(windows.map(window => window.executed.length), [0, 0, 0, 0, 0]);
    assert.deepEqual(devtools.executed, []);
});

test('an explicit https port - attached - counts as the default port', async () => {
    const window = contents('https://ktalk.ru:443/room');
    const app = application({ windows: [window] });

    await app.attach();

    assert.deepEqual(window.executed, [true]);
});

test('a destroyed window - attached - is skipped', async () => {
    const own = session();
    const window = contents('https://ktalk.ru/', { session: own });
    window.destroy();
    const app = application({ windows: [window] });

    await app.attach();

    assert.deepEqual(own.loads, []);
    assert.deepEqual(window.executed, []);
});

test('a window closed while its session loads the bundle - attached - runs no page code', async () => {
    let window = null;
    const own = session({ onLoad: () => window.destroy() });
    window = contents('https://ktalk.ru/', { session: own });
    const app = application({ windows: [window] });

    await app.attach();

    assert.deepEqual(own.loads, [app.directory]);
    assert.deepEqual(window.executed, []);
});

test('a page that navigated away before the code ran - attached - skips the bundle', async () => {
    const window = contents('https://ktalk.ru/', { location: 'https://example.org/' });
    const app = application({ windows: [window] });

    await app.attach();

    assert.deepEqual(window.executed, [true]);
    assert.equal(window.page.__PCM_WORKLET_URL__, undefined);
    assert.equal(window.page.__recogDesktopRuntime, undefined);
    assert.equal(app.record().state, 'ready');
});

for (const [outcome, name] of [['failed', 'reports failure'], [null, 'reports no state']]) {
    test(`a page whose bundle ${name} - attached - fails the attachment`, async () => {
        const app = application({ windows: [contents('https://ktalk.ru/', { outcome })] });

        await assert.rejects(app.attach(), { message: 'Recog page initialization failed' });

        assert.equal(app.record().state, 'failed');
    });
}

test('an attachment in progress - attached again - refuses the second one', async () => {
    const app = application();

    const first = app.attach();
    await assert.rejects(app.attach(), { message: 'Previous integration initialization did not finish' });

    await first;
});

test('a failed attachment - attached again - refuses until the application restarts', async () => {
    const app = application({ defaultSession: session({ api: 'none' }) });
    await assert.rejects(app.attach());

    await assert.rejects(app.attach(), { message: 'Previous integration initialization did not finish' });
});

test('a finished attachment - attached again - answers the first digest without listening twice', async () => {
    const app = application();
    await app.attach();

    const again = await vm.runInContext(`attachDesktop("/elsewhere", "other-digest", "/elsewhere/runtime.json", {})`, app.context);

    assert.deepEqual(plain(again), { app_version: '3.14.1', bundle: DIGEST });
    assert.equal(app.listeners['web-contents-created'].length, 1);
    assert.equal(app.record().watch, app.listeners['web-contents-created'][0]);
});

test('a window opened later - its page loads - gets the bundle', async () => {
    const app = application();
    await app.attach();
    const window = contents('https://ktalk.ru/room');

    await app.open(window);

    assert.deepEqual(window.executed, [true]);
    assert.equal(fs.existsSync(app.status), false);
});

test('a page that fails to initialize - its page loads - records the attachment incompatible', async () => {
    const app = application();
    await app.attach();

    await app.open(contents('https://ktalk.ru/room', { outcome: 'failed' }));

    const runtime = app.runtime();
    assert.equal(runtime.state, 'incompatible');
    assert.equal(runtime.reason, 'page_initialization_failed');
    assert.deepEqual(runtime.target, TARGET);
    assert.equal(runtime.app_version, '3.14.1');
    assert.ok(Math.abs(runtime.updated_at - Date.now() / 1000) < 60);
    assert.equal(fs.statSync(app.status).mode & 0o777, 0o600);
    assert.deepEqual(fs.readdirSync(app.directory).sort(), ['injected.js', 'runtime.json']);
});

test('a failed page - reloaded successfully - records the attachment ready again, once', async () => {
    const app = application();
    await app.attach();
    const window = contents('https://ktalk.ru/room', { outcome: 'failed' });
    await app.open(window);

    window.page.outcome = 'ready';
    await window.domReady();

    assert.equal(app.runtime().state, 'ready');
    assert.equal(app.runtime().reason, null);
    fs.rmSync(app.status);
    await window.domReady();
    assert.equal(fs.existsSync(app.status), false);
});

test('a failed page - a page that is not a call loads - keeps the failure', async () => {
    const app = application();
    await app.attach();
    await app.open(contents('https://ktalk.ru/room', { outcome: 'failed' }));

    await app.open(contents('https://example.org/'));

    assert.equal(app.runtime().state, 'incompatible');
});

test('an unwritable status file - a page fails and then recovers - keeps the failure to retry later', async () => {
    const app = application({ statusPath: path.join(os.tmpdir(), 'recog-bootstrap-missing', 'nested', 'runtime.json') });
    await app.attach();
    const window = contents('https://ktalk.ru/room', { outcome: 'failed' });

    await app.open(window);
    window.page.outcome = 'ready';
    await window.domReady();

    assert.equal(app.record().lastError, 'page_initialization_failed');
    assert.equal(fs.existsSync(app.status), false);
});

test('a main process without require - attached - takes require from the main module', async () => {
    const app = application({
        globals: fakeRequire => ({
            process: { pid: 77, mainModule: { require: fakeRequire } },
        }),
    });

    assert.deepEqual(plain(await app.attach()), { app_version: '3.14.1', bundle: DIGEST });
    assert.deepEqual(app.required, ['electron', 'node:fs']);
});

test('a main process without require or a main module - attached - creates require for the executable', async () => {
    const created = [];
    const app = application({
        globals: fakeRequire => ({
            process: {
                pid: 77,
                execPath: '/opt/ktalk/ktalk',
                getBuiltinModule: name => ({
                    createRequire(from) { created.push([name, from]); return fakeRequire; },
                }),
            },
        }),
    });

    assert.deepEqual(plain(await app.attach()), { app_version: '3.14.1', bundle: DIGEST });
    assert.deepEqual(created, [['module', '/opt/ktalk/ktalk']]);
});
