// A small DOM for the web UI's node:test suites: enough of elements, markup
// parsing, simple selectors and bubbling events to run the classic scripts of
// crates/recog-web/static against the real index.html and read back what a user sees.

const VOID_TAGS = new Set(['area', 'br', 'col', 'embed', 'hr', 'img', 'input', 'link', 'meta', 'source', 'wbr']);
const ENTITIES = { amp: '&', lt: '<', gt: '>', quot: '"', apos: "'", nbsp: ' ', times: '×', larr: '←' };

function decode(text) {
    return text.replace(/&(#x[0-9a-f]+|#\d+|[a-z]+);/gi, (whole, name) => {
        if (name[0] === '#') {
            const code = name[1] === 'x' || name[1] === 'X' ? parseInt(name.slice(2), 16) : parseInt(name.slice(1), 10);
            return String.fromCodePoint(code);
        }
        return ENTITIES[name] ?? whole;
    });
}

const escapeText = text => text.replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;');
const escapeAttribute = text => escapeText(text).replaceAll('"', '&quot;');
const kebab = name => name.replace(/[A-Z]/g, letter => `-${letter.toLowerCase()}`);
const camel = name => name.replace(/-([a-z])/g, (_, letter) => letter.toUpperCase());

class TextNode {
    constructor(text) {
        this.nodeType = 3;
        this.data = text;
        this.parentNode = null;
    }
    get textContent() { return this.data; }
    set textContent(value) { this.data = String(value); }
    remove() { this.parentNode?.removeChild(this); }
}

function parseStyle(text) {
    const style = {};
    for (const part of (text || '').split(';')) {
        const at = part.indexOf(':');
        if (at > 0) style[camel(part.slice(0, at).trim())] = part.slice(at + 1).trim();
    }
    return style;
}

class Element {
    constructor(document, tagName) {
        this.nodeType = 1;
        this.ownerDocument = document;
        this.tagName = tagName.toUpperCase();
        this.localName = tagName.toLowerCase();
        this.attributes = new Map();
        this.childNodes = [];
        this.parentNode = null;
        this.listeners = {};
        this.style = {};
        this.scrollTop = 0;
        this.scrollHeight = 0;
        this.focused = false;
        this.scrolledIntoView = null;
        this._value = null;
        this.checked = false;
        this.open = false;
        const element = this;
        this.dataset = new Proxy({}, {
            get: (_, key) => (typeof key === 'string' ? element.getAttribute(`data-${kebab(key)}`) ?? undefined : undefined),
            set: (_, key, value) => { element.setAttribute(`data-${kebab(key)}`, String(value)); return true; },
            deleteProperty: (_, key) => { element.removeAttribute(`data-${kebab(key)}`); return true; },
            has: (_, key) => element.hasAttribute(`data-${kebab(key)}`),
            ownKeys: () => [...element.attributes.keys()].filter(name => name.startsWith('data-')).map(name => camel(name.slice(5))),
            getOwnPropertyDescriptor: (_, key) => ({ enumerable: true, configurable: true, value: element.dataset[key] }),
        });
        this.classList = {
            add: (...names) => { const set = this._classes(); names.forEach(name => set.add(name)); this._writeClasses(set); },
            remove: (...names) => { const set = this._classes(); names.forEach(name => set.delete(name)); this._writeClasses(set); },
            contains: name => this._classes().has(name),
            toggle: (name, force) => {
                const set = this._classes();
                const on = force === undefined ? !set.has(name) : Boolean(force);
                if (on) set.add(name); else set.delete(name);
                this._writeClasses(set);
                return on;
            },
        };
    }

    _classes() {
        if (!this._classSet || this._classSource !== this.getAttribute('class')) {
            this._classSource = this.getAttribute('class');
            this._classSet = new Set((this._classSource || '').split(/\s+/).filter(Boolean));
        }
        return this._classSet;
    }

    _writeClasses(set = this._classSet) {
        this.setAttribute('class', [...set].join(' '));
    }

    getAttribute(name) { return this.attributes.has(name) ? this.attributes.get(name) : null; }
    setAttribute(name, value) {
        this.attributes.set(name, String(value));
        if (name === 'style') this.style = parseStyle(String(value));
    }
    removeAttribute(name) { this.attributes.delete(name); }
    hasAttribute(name) { return this.attributes.has(name); }
    toggleAttribute(name, force) {
        const on = force === undefined ? !this.hasAttribute(name) : Boolean(force);
        if (on) this.setAttribute(name, ''); else this.removeAttribute(name);
        return on;
    }

    get id() { return this.getAttribute('id') || ''; }
    set id(value) { this.setAttribute('id', value); }
    get className() { return this.getAttribute('class') || ''; }
    set className(value) { this.setAttribute('class', value); }
    get title() { return this.getAttribute('title') || ''; }
    set title(value) { this.setAttribute('title', value); }
    get href() { return this.getAttribute('href') || ''; }
    set href(value) { this.setAttribute('href', value); }
    get download() { return this.getAttribute('download') || ''; }
    set download(value) { this.setAttribute('download', value); }
    get type() { return this.getAttribute('type') || ''; }
    set type(value) { this.setAttribute('type', value); }
    get hidden() { return this.hasAttribute('hidden'); }
    set hidden(value) { this.toggleAttribute('hidden', Boolean(value)); }
    get disabled() { return this.hasAttribute('disabled'); }
    set disabled(value) { this.toggleAttribute('disabled', Boolean(value)); }

    get value() {
        if (this._value !== null) return this._value;
        if (this.localName === 'textarea') return this.textContent;
        if (this.localName === 'select') {
            const options = this.querySelectorAll('option');
            const chosen = options.find(option => option.hasAttribute('selected')) || options[0];
            return chosen ? chosen.value : '';
        }
        if (this.localName === 'option') return this.getAttribute('value') ?? this.textContent;
        return this.getAttribute('value') ?? '';
    }
    set value(value) { this._value = String(value); }

    get children() { return this.childNodes.filter(node => node.nodeType === 1); }
    get firstElementChild() { return this.children[0] || null; }
    get lastElementChild() { return this.children.at(-1) || null; }
    get parentElement() { return this.parentNode && this.parentNode.nodeType === 1 ? this.parentNode : null; }
    get isConnected() {
        let node = this;
        while (node.parentNode) node = node.parentNode;
        return node === this.ownerDocument.documentElement;
    }

    get textContent() { return this.childNodes.map(node => node.textContent).join(''); }
    set textContent(value) {
        this._detachChildren();
        if (value !== '' && value != null) this._adopt(new TextNode(String(value)));
    }
    get innerText() { return this.textContent; }
    set innerText(value) { this.textContent = value; }

    get innerHTML() { return this.childNodes.map(serialize).join(''); }
    set innerHTML(html) {
        this._detachChildren();
        for (const node of parse(this.ownerDocument, String(html))) this._adopt(node);
    }
    get outerHTML() { return serialize(this); }

    _detachChildren() {
        for (const node of this.childNodes) node.parentNode = null;
        this.childNodes = [];
    }

    _adopt(node, before = null) {
        if (node.nodeType === 11) {
            for (const child of [...node.childNodes]) this._adopt(child, before);
            return node;
        }
        node.parentNode?.removeChild(node);
        node.parentNode = this;
        const at = before ? this.childNodes.indexOf(before) : -1;
        if (at < 0) this.childNodes.push(node); else this.childNodes.splice(at, 0, node);
        return node;
    }

    appendChild(node) { return this._adopt(node); }
    append(...nodes) { nodes.forEach(node => this._adopt(typeof node === 'string' ? new TextNode(node) : node)); }
    prepend(...nodes) { nodes.reverse().forEach(node => this._adopt(typeof node === 'string' ? new TextNode(node) : node, this.childNodes[0] || null)); }
    insertBefore(node, before) { return this._adopt(node, before); }
    replaceChildren(...nodes) { this._detachChildren(); this.append(...nodes); }
    removeChild(node) {
        this.childNodes = this.childNodes.filter(child => child !== node);
        node.parentNode = null;
        return node;
    }
    remove() { this.parentNode?.removeChild(this); }
    contains(node) {
        for (let at = node; at; at = at.parentNode) if (at === this) return true;
        return false;
    }

    descendants() {
        const found = [];
        const walk = element => element.children.forEach(child => { found.push(child); walk(child); });
        walk(this);
        return found;
    }
    querySelectorAll(selector) { return select(this, selector); }
    querySelector(selector) { return select(this, selector)[0] || null; }
    matches(selector) { return splitList(selector).some(chain => matchesChain(this, chain)); }
    closest(selector) {
        for (let at = this; at && at.nodeType === 1; at = at.parentNode) if (at.matches(selector)) return at;
        return null;
    }

    addEventListener(type, listener) { (this.listeners[type] ||= []).push(listener); }
    removeEventListener(type, listener) {
        this.listeners[type] = (this.listeners[type] || []).filter(known => known !== listener);
    }
    dispatchEvent(event) {
        event.target ||= this;
        const path = [];
        for (let at = this; at; at = at.parentNode) path.push(at);
        if (path.at(-1) === this.ownerDocument.documentElement) path.push(this.ownerDocument);
        for (const at of event.bubbles ? path : path.slice(0, 1)) {
            if (event.propagationStopped) break;
            event.currentTarget = at;
            for (const listener of [...(at.listeners?.[event.type] || [])]) listener.call(at, event);
            // An onclick="…" attribute, run by the page that loaded the markup.
            const inline = at.nodeType === 1 && at.getAttribute(`on${event.type}`);
            if (inline && at.ownerDocument.runInlineHandler) at.ownerDocument.runInlineHandler(inline, at, event);
        }
        return !event.defaultPrevented;
    }
    click() {
        if (this.disabled) return;
        this.dispatchEvent(makeEvent('click'));
        const form = this.closest('form');
        if (form && this.localName === 'button' && (this.type || 'submit') === 'submit') form.requestSubmit();
    }
    requestSubmit() { this.dispatchEvent(makeEvent('submit')); }
    focus() { this.focused = true; this.ownerDocument.activeElement = this; }
    blur() { this.focused = false; }
    select() {}
    scrollIntoView(options) { this.scrolledIntoView = options ?? true; }
    scrollTo(options) { if (options && typeof options.top === 'number') this.scrollTop = options.top; }
    setPointerCapture() {}
    releasePointerCapture() {}
    getBoundingClientRect() { return { left: 0, top: 0, width: 0, height: 0, right: 0, bottom: 0 }; }
    showModal() { this.open = true; this.setAttribute('open', ''); }
    close() { this.open = false; this.removeAttribute('open'); this.dispatchEvent(makeEvent('close', { bubbles: false })); }
}

function makeEvent(type, init = {}) {
    return {
        type,
        bubbles: init.bubbles ?? true,
        target: null,
        currentTarget: null,
        defaultPrevented: false,
        propagationStopped: false,
        preventDefault() { this.defaultPrevented = true; },
        stopPropagation() { this.propagationStopped = true; },
        ...init,
    };
}

function serialize(node) {
    if (node.nodeType === 3) return escapeText(node.data);
    const attributes = [...node.attributes].map(([name, value]) => (value === '' ? ` ${name}` : ` ${name}="${escapeAttribute(value)}"`)).join('');
    if (VOID_TAGS.has(node.localName)) return `<${node.localName}${attributes}>`;
    return `<${node.localName}${attributes}>${node.childNodes.map(serialize).join('')}</${node.localName}>`;
}

function parseAttributes(element, text) {
    for (const [, name, double, single, bare] of text.matchAll(/([^\s=/]+)(?:\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s>]+)))?/g)) {
        element.setAttribute(name, decode(double ?? single ?? bare ?? ''));
    }
}

function parse(document, html) {
    const root = new Element(document, 'template');
    const stack = [root];
    const tokens = /<!--[\s\S]*?-->|<!doctype[^>]*>|<\/([a-zA-Z][\w-]*)\s*>|<([a-zA-Z][\w-]*)((?:[^>"']|"[^"]*"|'[^']*')*)>/gi;
    let last = 0;
    for (const token of html.matchAll(tokens)) {
        const parent = stack.at(-1);
        if (token.index > last) parent._adopt(new TextNode(decode(html.slice(last, token.index))));
        last = token.index + token[0].length;
        const [, closing, opening, rest] = token;
        if (closing) {
            const at = stack.map(element => element.localName).lastIndexOf(closing.toLowerCase());
            if (at > 0) stack.length = at;
        } else if (opening) {
            const element = new Element(document, opening);
            const selfClosing = /\/\s*$/.test(rest);
            parseAttributes(element, selfClosing ? rest.replace(/\/\s*$/, '') : rest);
            parent._adopt(element);
            if (!selfClosing && !VOID_TAGS.has(element.localName)) stack.push(element);
        }
    }
    if (last < html.length) stack.at(-1)._adopt(new TextNode(decode(html.slice(last))));
    const nodes = [...root.childNodes];
    root._detachChildren();
    return nodes;
}

function splitList(selector) {
    return selector.split(',').map(part => part.trim().split(/\s+/).filter(Boolean)).filter(chain => chain.length);
}

function matchesCompound(element, compound) {
    const parts = compound.match(/^[a-zA-Z][\w-]*|#[\w-]+|\.[\w-]+|\[[^\]]+\]/g) || [];
    if (parts.join('') !== compound) throw new Error(`fake_dom: unsupported selector ${compound}`);
    return parts.every(part => {
        if (part[0] === '#') return element.id === part.slice(1);
        if (part[0] === '.') return element.classList.contains(part.slice(1));
        if (part[0] === '[') {
            const [, name, value] = part.match(/^\[([^=\]]+)(?:="?([^"]*)"?)?\]$/);
            return value === undefined ? element.hasAttribute(name) : element.getAttribute(name) === value;
        }
        return element.localName === part.toLowerCase();
    });
}

function matchesChain(element, chain) {
    if (!matchesCompound(element, chain.at(-1))) return false;
    let at = element.parentNode;
    for (let index = chain.length - 2; index >= 0; index--) {
        while (at && at.nodeType === 1 && !matchesCompound(at, chain[index])) at = at.parentNode;
        if (!at || at.nodeType !== 1) return false;
        at = at.parentNode;
    }
    return true;
}

function select(scope, selector) {
    const chains = splitList(selector);
    return scope.descendants().filter(element => chains.some(chain => matchesChain(element, chain)));
}

class Fragment {
    constructor() { this.nodeType = 11; this.childNodes = []; }
}

function createDocument(html) {
    const document = {
        listeners: {},
        hidden: false,
        activeElement: null,
        addEventListener(type, listener) { (this.listeners[type] ||= []).push(listener); },
        removeEventListener(type, listener) {
            this.listeners[type] = (this.listeners[type] || []).filter(known => known !== listener);
        },
        dispatchEvent(event) {
            event.target ||= this;
            for (const listener of [...(this.listeners[event.type] || [])]) listener.call(this, event);
            return !event.defaultPrevented;
        },
        createElement: tag => new Element(document, tag),
        createTextNode: text => new TextNode(text),
        createDocumentFragment: () => new Fragment(),
        getElementById: id => document.documentElement.descendants().find(element => element.id === id) || null,
        querySelectorAll: selector => select(document.documentElement, selector),
        querySelector: selector => select(document.documentElement, selector)[0] || null,
    };
    document.documentElement = new Element(document, 'html');
    for (const node of parse(document, html)) {
        if (node.nodeType === 1 && node.localName === 'html') {
            for (const child of [...node.childNodes]) document.documentElement._adopt(child);
            for (const [name, value] of node.attributes) document.documentElement.setAttribute(name, value);
        } else {
            document.documentElement._adopt(node);
        }
    }
    document.head = document.documentElement.querySelector('head');
    document.body = document.documentElement.querySelector('body');
    return document;
}

module.exports = { createDocument, makeEvent, Element };
