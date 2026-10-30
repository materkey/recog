const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const path = require('node:path');

const sourcePath = path.join(__dirname, '../recog/web/static/app.js');
const source = fs.readFileSync(sourcePath, 'utf8');

const RECT_KEY = 'recog.copilot-panel.rect';
const PANEL_IDS = [
    'copilot-panel', 'copilot-panel-header', 'copilot-panel-chip', 'copilot-detach-btn', 'copilot-stop-btn',
    'copilot-close-btn', 'copilot-messages', 'copilot-input', 'copilot-send-btn',
];

function element(tags = []) {
    const listeners = {};
    const classes = new Set();
    const self = {
        style: {},
        dataset: {},
        value: '',
        scrollHeight: 0,
        classList: {
            add: name => classes.add(name),
            remove: name => classes.delete(name),
            contains: name => classes.has(name),
            toggle: (name, on) => (on ? classes.add(name) : classes.delete(name)),
        },
        addEventListener(type, listener) {
            (listeners[type] ||= []).push(listener);
        },
        fire(type, event = {}) {
            return Promise.all((listeners[type] || []).map(listener => listener(event)));
        },
        closest: selector => (selector.split(',').some(part => tags.includes(part.trim())) ? self : null),
        querySelector: () => element(),
        setPointerCapture() {},
        appendChild() {},
        remove() {},
    };
    return self;
}

function page({ saved = null, viewport = { width: 1400, height: 800 }, pictureInPicture = true, hash = '#/live' } = {}) {
    const panelEls = Object.fromEntries(PANEL_IDS.map(id => [id, element()]));
    panelEls['copilot-panel'].getBoundingClientRect = () => ({ width: 420, height: 560 });
    const pageEls = {};
    const stored = new Map(saved === null ? [] : [[RECT_KEY, saved]]);
    const sockets = [];
    const windowListeners = {};
    const appended = [];
    const state = { detached: false, requested: [], refuse: null, pip: null };

    function FakeWebSocket(url) {
        this.url = url;
        this.readyState = 1;
        this.sent = [];
        this.send = raw => this.sent.push(JSON.parse(raw));
        this.close = () => {
            this.closed = true;
            this.onclose?.();
        };
        sockets.push(this);
    }
    FakeWebSocket.OPEN = 1;

    const documentPictureInPicture = {
        async requestWindow(size) {
            state.requested.push({ ...size });
            if (state.refuse) throw new Error(state.refuse);
            const pipListeners = {};
            const links = [];
            state.pip = {
                links,
                document: {
                    head: { append: link => links.push(link) },
                    body: {
                        append(node) {
                            assert.equal(node, panelEls['copilot-panel']);
                            state.detached = true;
                        },
                    },
                    createElement: () => ({}),
                    getElementById: id => (state.detached ? panelEls[id] : undefined),
                },
                addEventListener(type, listener) {
                    pipListeners[type] = listener;
                },
                close() {
                    state.pip.browserClose();
                },
                browserClose() {
                    pipListeners.pagehide();
                },
            };
            return state.pip;
        },
    };

    const context = vm.createContext({
        console: { error() {} },
        crypto: { randomUUID: () => 'id' },
        setTimeout: () => 0,
        localStorage: {
            getItem: key => stored.get(key) ?? null,
            setItem: (key, value) => stored.set(key, value),
        },
        WebSocket: FakeWebSocket,
        window: {
            location: { hash, protocol: 'http:', host: 'localhost' },
            innerWidth: viewport.width,
            innerHeight: viewport.height,
            addEventListener(type, listener) {
                windowListeners[type] = listener;
            },
            ...(pictureInPicture ? { documentPictureInPicture } : {}),
        },
        document: {
            addEventListener() {},
            createElement: () => element(),
            querySelectorAll: selector => (selector === 'link[rel="stylesheet"]' ? [{ href: 'http://h/static/style.css' }] : []),
            body: {
                append(node) {
                    assert.equal(node, panelEls['copilot-panel']);
                    state.detached = false;
                    appended.push(node);
                },
            },
            getElementById(id) {
                if (PANEL_IDS.includes(id)) return state.detached ? null : panelEls[id];
                return (pageEls[id] ||= element());
            },
        },
    });
    vm.runInContext(source, context, { filename: sourcePath });
    const evaluate = code => vm.runInContext(code, context);
    evaluate('initCopilotPanel()');

    const panel = panelEls['copilot-panel'];
    const header = element(['.copilot-panel-header']);
    const place = () => {
        const { left, top, width, height } = panel.style;
        return { left, top, width, height };
    };
    const gesture = (target, from, to, button = 0) => {
        panel.fire('pointerdown', { button, pointerId: 1, clientX: from[0], clientY: from[1], target });
        panel.fire('pointermove', { pointerId: 1, clientX: to[0], clientY: to[1], target });
        panel.fire('lostpointercapture', { pointerId: 1, target });
    };
    const edgeHandle = (edge) => {
        const handle = element(['.copilot-resize-handle']);
        handle.dataset.edge = edge;
        return handle;
    };
    return {
        evaluate, panel, header, place, gesture, edgeHandle, state, sockets, stored, appended, panelEls,
        open: () => evaluate('openCopilotPanel()'),
        router: () => evaluate('router'),
        detach: () => panelEls['copilot-detach-btn'].fire('click'),
        dialog: () => JSON.parse(evaluate('JSON.stringify(copilotState.messages.map(({ kind, text }) => ({ kind, text })))')),
        resizeWindow(width, height) {
            context.window.innerWidth = width;
            context.window.innerHeight = height;
            windowListeners.resize();
        },
    };
}

const px = (left, top, width, height) => ({
    left: `${left}px`, top: `${top}px`, width: `${width}px`, height: `${height}px`,
});
const savedRect = (left, top, width, height) => JSON.stringify({ left, top, width, height });

test('no saved place - panel opens - bottom right corner at the default size', () => {
    const p = page();
    p.open();
    assert.deepEqual(p.place(), px(956, 216, 420, 560));
});

test('open panel - header dragged - follows the pointer and the place is saved', () => {
    const p = page();
    p.open();
    p.gesture(p.header, [1000, 230], [700, 130]);
    assert.deepEqual(p.place(), px(656, 116, 420, 560));
    assert.deepEqual(JSON.parse(p.stored.get(RECT_KEY)), { left: 656, top: 116, width: 420, height: 560 });
});

test('saved place - page reloaded and panel opens - back where it was', () => {
    const p = page({ saved: savedRect(100, 50, 500, 600) });
    p.open();
    assert.deepEqual(p.place(), px(100, 50, 500, 600));
});

test('open panel - header dragged past the window corner - stays fully on screen', () => {
    const p = page();
    p.open();
    p.gesture(p.header, [1000, 230], [5000, 5000]);
    assert.deepEqual(p.place(), px(972, 232, 420, 560));
    p.gesture(p.header, [1000, 300], [-5000, -5000]);
    assert.deepEqual(p.place(), px(8, 8, 420, 560));
});

test('open panel - button in the header pressed and moved - panel stays put', () => {
    const p = page();
    p.open();
    const button = element(['button', '.copilot-panel-header']);
    p.gesture(button, [1000, 230], [700, 130]);
    assert.deepEqual(p.place(), px(956, 216, 420, 560));
});

test('open panel - secondary button drags the header - panel stays put', () => {
    const p = page();
    p.open();
    p.gesture(p.header, [1000, 230], [700, 130], 2);
    assert.deepEqual(p.place(), px(956, 216, 420, 560));
});

test('open panel - pointer pressed outside header and handles - panel stays put', () => {
    const p = page();
    p.open();
    p.gesture(element(), [1000, 400], [700, 130]);
    assert.deepEqual(p.place(), px(956, 216, 420, 560));
});

test('open panel - header clicked without moving - nothing is saved and later moves are ignored', () => {
    const p = page();
    p.open();
    p.panel.fire('pointerdown', { button: 0, pointerId: 1, clientX: 1000, clientY: 230, target: p.header });
    p.panel.fire('lostpointercapture', { pointerId: 1, target: p.header });
    p.panel.fire('pointermove', { pointerId: 1, clientX: 5, clientY: 5, target: p.header });
    assert.deepEqual(p.place(), px(956, 216, 420, 560));
    assert.equal(p.stored.has(RECT_KEY), false);
});

test('open panel - another pointer moves during a drag - only the dragging pointer counts', () => {
    const p = page();
    p.open();
    p.panel.fire('pointerdown', { button: 0, pointerId: 1, clientX: 1000, clientY: 230, target: p.header });
    p.panel.fire('pointermove', { pointerId: 2, clientX: 5, clientY: 5, target: p.header });
    assert.deepEqual(p.place(), px(956, 216, 420, 560));
});

test('open panel - north west corner dragged out - grows while right and bottom edges stay', () => {
    const p = page();
    p.open();
    p.gesture(p.edgeHandle('nw'), [956, 216], [756, 116]);
    assert.deepEqual(p.place(), px(756, 116, 620, 660));
});

test('open panel - south east corner dragged in - shrinks while left and top edges stay', () => {
    const p = page({ saved: savedRect(100, 50, 500, 600) });
    p.open();
    p.gesture(p.edgeHandle('se'), [600, 650], [550, 600]);
    assert.deepEqual(p.place(), px(100, 50, 450, 550));
});

test('open panel - edges dragged far inward - stop at the minimum size', () => {
    const p = page({ saved: savedRect(100, 50, 500, 600) });
    p.open();
    p.gesture(p.edgeHandle('se'), [600, 650], [0, 0]);
    assert.deepEqual(p.place(), px(100, 50, 300, 320));
    p.gesture(p.edgeHandle('nw'), [100, 50], [5000, 5000]);
    assert.deepEqual(p.place(), px(100, 50, 300, 320));
});

test('open panel - edges dragged far outward - stop at the window edge', () => {
    const p = page({ saved: savedRect(100, 50, 500, 600) });
    p.open();
    p.gesture(p.edgeHandle('se'), [600, 650], [5000, 5000]);
    assert.deepEqual(p.place(), px(100, 50, 1292, 742));
    p.gesture(p.edgeHandle('nw'), [100, 50], [-5000, -5000]);
    assert.deepEqual(p.place(), px(8, 8, 1384, 784));
});

test('open panel - single edges dragged - only that edge moves', () => {
    const p = page({ saved: savedRect(100, 50, 500, 600) });
    p.open();
    p.gesture(p.edgeHandle('e'), [600, 300], [650, 999]);
    assert.deepEqual(p.place(), px(100, 50, 550, 600));
    p.gesture(p.edgeHandle('s'), [300, 650], [999, 680]);
    assert.deepEqual(p.place(), px(100, 50, 550, 630));
    p.gesture(p.edgeHandle('w'), [100, 300], [60, 999]);
    assert.deepEqual(p.place(), px(60, 50, 590, 630));
    p.gesture(p.edgeHandle('n'), [300, 50], [999, 20]);
    assert.deepEqual(p.place(), px(60, 20, 590, 660));
});

test('place chosen on a large window - window shrinks then grows back - fits, then returns', () => {
    const p = page({ saved: savedRect(800, 200, 500, 500) });
    p.open();
    p.resizeWindow(600, 400);
    assert.deepEqual(p.place(), px(92, 8, 500, 384));
    p.resizeWindow(1400, 800);
    assert.deepEqual(p.place(), px(800, 200, 500, 500));
});

for (const [name, saved] of [
    ['not JSON', '{oops'],
    ['not an object', 'false'],
    ['with a missing size', JSON.stringify({ left: 1, top: 2, width: 3 })],
    ['with text instead of numbers', savedRect('1', '2', '3', '4')],
]) {
    test(`saved place ${name} - panel opens - default place`, () => {
        const p = page({ saved });
        p.open();
        assert.deepEqual(p.place(), px(956, 216, 420, 560));
    });
}

test('storage unavailable - place saved and read - panel keeps working', () => {
    const p = page();
    p.open();
    p.evaluate('localStorage').setItem = () => {
        throw new Error('denied');
    };
    p.gesture(p.header, [1000, 230], [700, 130]);
    assert.deepEqual(p.place(), px(656, 116, 420, 560));
});

test('focused header - arrow keys - move the panel by a step and save the place', () => {
    const p = page();
    p.open();
    const headerEl = p.panelEls['copilot-panel-header'];
    let prevented = 0;
    headerEl.fire('keydown', { key: 'ArrowLeft', target: headerEl, currentTarget: headerEl, preventDefault: () => prevented++ });
    headerEl.fire('keydown', { key: 'ArrowUp', target: headerEl, currentTarget: headerEl, preventDefault: () => prevented++ });
    assert.deepEqual(p.place(), px(941, 201, 420, 560));
    assert.equal(prevented, 2);
    assert.deepEqual(JSON.parse(p.stored.get(RECT_KEY)), { left: 941, top: 201, width: 420, height: 560 });
});

test('focused header - shift and arrow keys - change the size', () => {
    const p = page({ saved: savedRect(100, 50, 500, 600) });
    p.open();
    const headerEl = p.panelEls['copilot-panel-header'];
    const key = name => ({ key: name, shiftKey: true, target: headerEl, currentTarget: headerEl, preventDefault() {} });
    headerEl.fire('keydown', key('ArrowRight'));
    headerEl.fire('keydown', key('ArrowDown'));
    assert.deepEqual(p.place(), px(100, 50, 515, 615));
});

test('focus on a button in the header - arrow key or another key - panel stays put', () => {
    const p = page();
    p.open();
    const headerEl = p.panelEls['copilot-panel-header'];
    headerEl.fire('keydown', { key: 'ArrowLeft', target: element(), currentTarget: headerEl, preventDefault() {} });
    headerEl.fire('keydown', { key: 'a', target: headerEl, currentTarget: headerEl, preventDefault() {} });
    assert.deepEqual(p.place(), px(956, 216, 420, 560));
});

test('browser without the floating window - page loads - no detach button', () => {
    const p = page({ pictureInPicture: false });
    assert.equal(p.panelEls['copilot-detach-btn'].hidden, true);
});

test('browser with the floating window - page loads - detach button available', () => {
    const p = page();
    const button = p.panelEls['copilot-detach-btn'];
    assert.notEqual(button.hidden, true);
    assert.equal(button.textContent, 'Открепить');
});

test('open panel - detach pressed - moves to a window of its size with the page styles', async () => {
    const p = page();
    p.open();
    await p.detach();
    assert.deepEqual(p.state.requested, [{ width: 420, height: 560 }]);
    assert.equal(p.state.detached, true);
    assert.equal(p.state.pip.links[0].href, 'http://h/static/style.css');
    assert.equal(p.state.pip.links[0].rel, 'stylesheet');
    assert.equal(p.panel.classList.contains('copilot-panel-detached'), true);
    assert.deepEqual(p.place(), { left: '', top: '', width: '', height: '' });
    assert.equal(p.panelEls['copilot-detach-btn'].textContent, 'Вернуть');
    assert.equal(p.panelEls['copilot-panel-header'].tabIndex, -1);
});

test('detached panel - question typed there and sent - goes out and shows in the window', async () => {
    const p = page();
    p.router().handleRoute();
    p.open();
    await p.detach();
    p.panelEls['copilot-input'].value = 'к чему ведёт';
    await p.panelEls['copilot-send-btn'].fire('click');
    assert.equal(p.sockets.at(-1).sent.at(-1).text, 'к чему ведёт');
    assert.deepEqual(p.dialog()[0], { kind: 'user', text: 'к чему ведёт' });
    assert.equal(p.panelEls['copilot-input'].value, '');
});

test('detached panel - agent state changes - chip in the window follows', async () => {
    const p = page();
    p.open();
    await p.detach();
    p.evaluate('handleCopilotAgentState')('online');
    assert.match(p.panelEls['copilot-panel-chip'].textContent, /online/);
    assert.equal(p.panelEls['copilot-stop-btn'].disabled, false);
});

test('detached panel - gestures and arrow keys - nothing moves', async () => {
    const p = page();
    p.open();
    await p.detach();
    p.gesture(p.header, [10, 10], [50, 50]);
    const headerEl = p.panelEls['copilot-panel-header'];
    headerEl.fire('keydown', { key: 'ArrowLeft', target: headerEl, currentTarget: headerEl, preventDefault() {} });
    p.resizeWindow(900, 700);
    assert.deepEqual(p.place(), { left: '', top: '', width: '', height: '' });
    assert.equal(p.stored.has(RECT_KEY), false);
});

test('detached panel - browser closes the window - panel back on the page, open, in its place', async () => {
    const p = page({ saved: savedRect(100, 50, 500, 600) });
    p.open();
    await p.detach();
    p.state.pip.browserClose();
    assert.equal(p.state.detached, false);
    assert.deepEqual(p.appended, [p.panel]);
    assert.equal(p.panel.classList.contains('copilot-panel-detached'), false);
    assert.equal(p.panel.style.display, 'flex');
    assert.deepEqual(p.place(), px(100, 50, 500, 600));
    assert.equal(p.panelEls['copilot-detach-btn'].textContent, 'Открепить');
    assert.equal(p.panelEls['copilot-panel-header'].tabIndex, 0);
});

test('detached panel - return pressed - panel back on the page and still open', async () => {
    const p = page();
    p.open();
    await p.detach();
    await p.detach();
    assert.equal(p.state.detached, false);
    assert.equal(p.panel.style.display, 'flex');
    assert.equal(p.state.requested.length, 1);
});

test('detached panel - close pressed in the window - window closes and the panel is hidden', async () => {
    const p = page();
    p.open();
    await p.detach();
    await p.panelEls['copilot-close-btn'].fire('click');
    assert.equal(p.state.detached, false);
    assert.equal(p.panel.style.display, 'none');
});

test('attached panel - stop pressed - stop request goes out', async () => {
    const p = page();
    p.router().handleRoute();
    p.open();
    await p.panelEls['copilot-stop-btn'].fire('click');
    assert.equal(p.sockets.at(-1).sent.at(-1).type, 'copilot_stop_request');
});

test('attached panel on live - another view opened - stream closes', () => {
    const p = page();
    p.router().handleRoute();
    p.router().showView('meetings');
    assert.equal(p.sockets[0].closed, true);
});

test('detached panel - another view opened - stream stays so the window keeps updating', async () => {
    const p = page();
    p.router().handleRoute();
    p.open();
    await p.detach();
    p.router().showView('meetings');
    assert.equal(p.sockets[0].closed, undefined);
    assert.equal(p.evaluate('views').live.ws, p.sockets[0]);
});

test('detached panel on another view - live opened again - one stream, the old one closed', async () => {
    const p = page();
    p.router().handleRoute();
    p.open();
    await p.detach();
    p.router().showView('meetings');
    p.router().handleRoute();
    assert.equal(p.sockets.length, 2);
    assert.equal(p.sockets[0].closed, true);
    assert.equal(p.evaluate('views').live.ws, p.sockets[1]);
});

test('detached panel on another view - window closed - stream closes with it', async () => {
    const p = page();
    p.router().handleRoute();
    p.open();
    await p.detach();
    p.evaluate('window').location.hash = '#/';
    p.state.pip.browserClose();
    assert.equal(p.sockets[0].closed, true);
    assert.equal(p.evaluate('views').live.ws, null);
});

test('detached panel on live - window closed - stream stays', async () => {
    const p = page();
    p.router().handleRoute();
    p.open();
    await p.detach();
    p.state.pip.browserClose();
    assert.equal(p.sockets[0].closed, undefined);
});

test('panel open on another view - detach pressed - stream opens so the window can update', async () => {
    const p = page({ hash: '#/' });
    p.open();
    await p.detach();
    assert.equal(p.sockets.length, 1);
});

test('browser refuses the window - detach pressed - says so and the panel stays on the page', async () => {
    const p = page();
    p.open();
    p.state.refuse = 'needs a click';
    await p.detach();
    assert.equal(p.state.detached, false);
    assert.equal(p.panelEls['copilot-detach-btn'].textContent, 'Открепить');
    assert.deepEqual(p.dialog(), [{ kind: 'error', text: 'Не удалось открепить окно: needs a click' }]);
});
