const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const path = require('node:path');

const sourcePath = path.join(__dirname, '../recog/web/static/app.js');
const source = fs.readFileSync(sourcePath, 'utf8');

function tab() {
    const dot = { hidden: true };
    const input = { value: '', disabled: false };
    const sent = [];
    const elements = { 'copilot-unread-dot': dot, 'copilot-input': input };
    const context = vm.createContext({
        console: { error() {} },
        crypto: { randomUUID: () => 'own' },
        localStorage: { getItem: () => 'false', setItem() {} },
        window: {},
        WebSocket: { OPEN: 1 },
        document: { addEventListener() {}, getElementById: id => elements[id] },
    });
    vm.runInContext(source, context, { filename: sourcePath });
    const evaluate = code => vm.runInContext(code, context);
    return {
        receive: frame => evaluate('views').live.handleMessage(frame),
        ask(text) {
            input.value = text;
            evaluate('views').live.ws = { readyState: 1, send: raw => sent.push(JSON.parse(raw)) };
            evaluate('submitCopilotChat()');
            return sent.at(-1).request_id;
        },
        dialog: () => JSON.parse(evaluate(
            'JSON.stringify(copilotState.messages.map(({ kind, text, time }) => ({ kind, text, time })))'
        )),
        dot,
    };
}

const CREATED_AT = Date.UTC(2026, 9, 30, 18, 17, 19);
const time = new Date(CREATED_AT).toLocaleTimeString('ru-RU', {
    hour: '2-digit', minute: '2-digit', second: '2-digit',
});
const question = {
    type: 'copilot_chat_echo', dialog_id: 'd1', request_id: 'r1', text: 'к чему ведёт', created_at: CREATED_AT,
};
const reply = {
    type: 'copilot_reply', dialog_id: 'd2', request_id: 'r1', kind: 'assistant', text: 'к выводам', created_at: CREATED_AT,
};
const askedAndAnswered = [
    { kind: 'user', text: 'к чему ведёт', time },
    { kind: 'assistant', text: 'к выводам', time },
];

test('dialog from another tab - new tab connects - shows it with original times and no unread dot', () => {
    const t = tab();
    t.receive({ type: 'copilot_history', frames: [question, reply] });
    assert.deepEqual(t.dialog(), askedAndAnswered);
    assert.equal(t.dot.hidden, true);
});

test('open tab - another tab asks and gets a reply - shows both', () => {
    const t = tab();
    t.receive(question);
    t.receive(reply);
    assert.deepEqual(t.dialog(), askedAndAnswered);
});

test('tab with the dialog - reconnects and gets the history - shows nothing twice', () => {
    const t = tab();
    t.receive(question);
    t.receive(reply);
    t.receive({ type: 'copilot_history', frames: [question, reply] });
    assert.deepEqual(t.dialog(), askedAndAnswered);
});

test('asking tab - own question is echoed and answered - one question and one resolved answer', () => {
    const t = tab();
    const requestId = t.ask('к чему ведёт');
    t.receive({ ...question, request_id: requestId });
    t.receive({ ...reply, request_id: requestId });
    assert.deepEqual(t.dialog().map(m => [m.kind, m.text]), [
        ['user', 'к чему ведёт'],
        ['assistant', 'к выводам'],
    ]);
});

test('closed panel - new reply arrives after the history - lights the unread dot', () => {
    const t = tab();
    t.receive({ type: 'copilot_history', frames: [question] });
    t.receive(reply);
    assert.equal(t.dot.hidden, false);
});

test('page reloaded before the frontend service restarted - replies without dialog_id - shows every reply', () => {
    const t = tab();
    t.receive({ type: 'copilot_reply', request_id: 'r1', kind: 'assistant', text: 'первый', created_at: CREATED_AT });
    t.receive({ type: 'copilot_reply', request_id: 'r2', kind: 'assistant', text: 'второй', created_at: CREATED_AT });
    assert.deepEqual(t.dialog().map(m => m.text), ['первый', 'второй']);
});
