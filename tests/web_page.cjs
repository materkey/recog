// Loads the web UI's classic scripts into one vm context over the real
// index.html (tests/fake_dom.cjs), with fakes for what the browser and the
// server provide: fetch, WebSocket, EventSource, Audio, storage, the clipboard,
// downloads and timers. Tests drive it the way a user and the server do and
// read the page back.

const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const { createDocument, makeEvent } = require('./fake_dom.cjs');

const STATIC = path.join(__dirname, '../crates/recog-web/static');
const MARKUP = fs.readFileSync(path.join(STATIC, 'index.html'), 'utf8');
const ALL_SCRIPTS = ['datepicker.js', 'user-profile.js', 'ai-settings.js', 'desktop-status.js', 'meeting-chat.js', 'app.js'];

const flush = () => new Promise(resolve => setImmediate(resolve));

function json(data, { status = 200 } = {}) {
    // A fresh copy per read, as a parsed response body is.
    return { ok: status >= 200 && status < 300, status, json: async () => structuredClone(data), text: async () => JSON.stringify(data) };
}

function fakeTimers() {
    let now = 0;
    let nextId = 1;
    const pending = new Map();
    const add = (callback, delay, repeat) => {
        const id = nextId++;
        pending.set(id, { callback, at: now + (Number(delay) || 0), every: repeat ? Math.max(1, Number(delay) || 0) : 0 });
        return id;
    };
    return {
        setTimeout: (callback, delay) => add(callback, delay, false),
        setInterval: (callback, delay) => add(callback, delay, true),
        clearTimeout: id => pending.delete(id),
        clearInterval: id => pending.delete(id),
        pending: () => [...pending.values()].map(({ at, every }) => ({ in: at - now, every })),
        // Runs every timer due within `ms`, in time order, timers they add included.
        advance(ms) {
            const until = now + ms;
            for (;;) {
                const due = [...pending].filter(([, timer]) => timer.at <= until).sort((a, b) => a[1].at - b[1].at)[0];
                if (!due) break;
                const [id, timer] = due;
                now = timer.at;
                if (timer.every) timer.at += timer.every; else pending.delete(id);
                timer.callback();
            }
            now = until;
        },
    };
}

function loadPage({
    hash = '#/',
    fetch = async () => json({}),
    local = {},
    session = {},
    scripts = ALL_SCRIPTS,
    ready = false,
    confirm = () => true,
    pictureInPicture = null,
    clipboardFails = false,
} = {}) {
    const document = createDocument(MARKUP);
    const timers = fakeTimers();
    const requests = [];
    const sockets = [];
    const eventSources = [];
    const audios = [];
    const clipboard = [];
    const downloads = [];
    const objectUrls = new Map();
    const alerts = [];
    const confirms = [];
    const errors = [];
    const windowListeners = {};
    const storage = (initial) => {
        const items = new Map(Object.entries(initial));
        return {
            items,
            getItem: key => (items.has(key) ? items.get(key) : null),
            setItem: (key, value) => items.set(key, String(value)),
            removeItem: key => items.delete(key),
        };
    };

    function FakeWebSocket(url) {
        this.url = url;
        this.readyState = FakeWebSocket.CONNECTING;
        this.sent = [];
        this.closed = false;
        sockets.push(this);
    }
    FakeWebSocket.CONNECTING = 0;
    FakeWebSocket.OPEN = 1;
    FakeWebSocket.CLOSED = 3;
    FakeWebSocket.prototype.send = function send(raw) { this.sent.push(JSON.parse(raw)); };
    FakeWebSocket.prototype.close = function close() {
        if (this.closed) return;
        this.closed = true;
        this.readyState = FakeWebSocket.CLOSED;
        this.onclose?.({});
    };
    FakeWebSocket.prototype.open = function open() {
        this.readyState = FakeWebSocket.OPEN;
        this.onopen?.({});
    };
    FakeWebSocket.prototype.receive = function receive(frame) {
        this.onmessage?.({ data: JSON.stringify(frame) });
    };

    function FakeEventSource(url) {
        this.url = url;
        this.closed = false;
        this.listeners = {};
        eventSources.push(this);
    }
    FakeEventSource.prototype.addEventListener = function addEventListener(type, listener) {
        (this.listeners[type] ||= []).push(listener);
    };
    FakeEventSource.prototype.close = function close() { this.closed = true; };
    FakeEventSource.prototype.emit = function emit(type, data) {
        const event = { data: typeof data === 'string' ? data : JSON.stringify(data) };
        if (type === 'message') this.onmessage?.(event);
        if (type === 'error') this.onerror?.(event);
        for (const listener of this.listeners[type] || []) listener(event);
    };

    function FakeAudio(src) {
        this.src = src;
        this.paused = true;
        this.currentTime = 0;
        this.listeners = {};
        this.playCalls = 0;
        audios.push(this);
    }
    FakeAudio.prototype.addEventListener = function addEventListener(type, listener) {
        (this.listeners[type] ||= []).push(listener);
    };
    FakeAudio.prototype.removeEventListener = function removeEventListener(type, listener) {
        this.listeners[type] = (this.listeners[type] || []).filter(known => known !== listener);
    };
    FakeAudio.prototype.emit = function emit(type) {
        if (type === 'ended' || type === 'pause') this.paused = true;
        this[`on${type}`]?.({});
        for (const listener of this.listeners[type] || []) listener({});
    };
    FakeAudio.prototype.play = function play() {
        this.playCalls++;
        this.paused = false;
        return this.playResult || Promise.resolve();
    };
    FakeAudio.prototype.pause = function pause() { this.paused = true; };
    FakeAudio.prototype.load = function load() {};

    let objectUrlId = 0;
    const context = {
        console: { log() {}, warn() {}, error: (...args) => errors.push(args) },
        document,
        location: { hash, protocol: 'http:', host: 'localhost:9080', origin: 'http://localhost:9080', href: `http://localhost:9080/${hash}` },
        localStorage: storage(local),
        sessionStorage: storage(session),
        navigator: {
            clipboard: {
                writeText: text => {
                    if (clipboardFails) return Promise.reject(new Error('denied'));
                    clipboard.push(text);
                    return Promise.resolve();
                },
            },
        },
        fetch: (url, options = {}) => {
            requests.push({ url: String(url), method: options.method || 'GET', body: options.body, options });
            return Promise.resolve().then(() => fetch(String(url), options));
        },
        WebSocket: FakeWebSocket,
        EventSource: FakeEventSource,
        Audio: FakeAudio,
        Blob: class Blob {
            constructor(parts, options = {}) { this.parts = parts; this.type = options.type || ''; }
            text() { return Promise.resolve(this.parts.join('')); }
        },
        URL: Object.assign(function URL(...args) { return new globalThis.URL(...args); }, {
            createObjectURL: blob => {
                const url = `blob:${++objectUrlId}`;
                objectUrls.set(url, blob);
                return url;
            },
            revokeObjectURL: url => objectUrls.delete(url),
        }),
        URLSearchParams,
        AbortController,
        TextEncoder,
        TextDecoder,
        Intl,
        crypto: { randomUUID: (() => { let n = 0; return () => `uuid-${++n}`; })() },
        structuredClone,
        queueMicrotask,
        setTimeout: timers.setTimeout,
        clearTimeout: timers.clearTimeout,
        setInterval: timers.setInterval,
        clearInterval: timers.clearInterval,
        requestAnimationFrame: callback => timers.setTimeout(callback, 16),
        innerWidth: 1400,
        innerHeight: 800,
        confirm: message => { confirms.push(message); return confirm(message); },
        alert: message => alerts.push(message),
        addEventListener(type, listener) { (windowListeners[type] ||= []).push(listener); },
        removeEventListener(type, listener) {
            windowListeners[type] = (windowListeners[type] || []).filter(known => known !== listener);
        },
        dispatchEvent(event) {
            for (const listener of [...(windowListeners[event.type] || [])]) listener(event);
            return true;
        },
    };
    if (pictureInPicture) context.documentPictureInPicture = pictureInPicture;
    context.window = context;
    context.self = context;
    vm.createContext(context);
    const evaluate = code => vm.runInContext(code, context);

    document.runInlineHandler = (code, element, event) =>
        vm.runInContext(`(function (event) { ${code} })`, context).call(element, event);

    // A click on <a download> is the page handing the user a file.
    const createElement = document.createElement;
    document.createElement = tag => {
        const element = createElement(tag);
        if (element.localName === 'a') {
            element.addEventListener('click', () => {
                if (element.download) downloads.push({ name: element.download, blob: objectUrls.get(element.href) });
            });
        }
        return element;
    };

    for (const name of scripts) {
        const file = path.join(STATIC, name);
        vm.runInContext(fs.readFileSync(file, 'utf8'), context, { filename: file });
    }
    if (ready) document.dispatchEvent(makeEvent('DOMContentLoaded'));

    const $ = id => {
        const element = document.getElementById(id);
        if (!element) throw new Error(`No element #${id}`);
        return element;
    };
    const text = node => (node.textContent || '').replace(/\s+/g, ' ').trim();
    // Text as read across element boundaries: each element's text a separate word.
    const words = node => {
        const parts = [];
        const walk = at => (at.nodeType === 3 ? parts.push(at.data) : at.childNodes.forEach(walk));
        walk(node);
        return parts.join(' ').replace(/\s+/g, ' ').trim();
    };
    return {
        context, document, window: context, evaluate, timers, requests, sockets, eventSources, audios,
        clipboard, downloads, alerts, confirms, errors, $, text, words, flush,
        setHash(next) {
            context.location.hash = next;
            context.dispatchEvent({ type: 'hashchange' });
        },
        socket: () => sockets.at(-1),
    };
}

module.exports = { loadPage, json, flush, makeEvent };
