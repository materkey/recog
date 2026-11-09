const { test } = require('node:test');
const assert = require('node:assert/strict');
const { loadPage } = require('./web_page.cjs');

// The live view with its stream open, as after opening #/live.
function livePage() {
    const page = loadPage({ hash: '#/live', ready: true });
    const socket = page.socket();
    socket.open();
    const sentOf = type => socket.sent.filter(frame => frame.type === type);
    return {
        ...page,
        status: frame => socket.receive({ type: 'copilot_status', ...frame }),
        bubbles: () => page.document.querySelectorAll('#copilot-messages .copilot-msg').map(el => ({
            kind: el.className.replace(/.*copilot-msg-(\w+).*/, '$1'),
            text: page.text(el.querySelector('.copilot-msg-text')),
        })),
        chip: () => page.text(page.$('copilot-chip')),
        input: () => ({ disabled: page.$('copilot-input').disabled, placeholder: page.$('copilot-input').placeholder }),
        stopButton: () => ({ text: page.text(page.$('copilot-stop-btn')), disabled: page.$('copilot-stop-btn').disabled }),
        spawn() {
            page.$('copilot-btn').click();
            return sentOf('copilot_spawn_request').at(-1).request_id;
        },
        stop() {
            page.$('copilot-stop-btn').click();
            return sentOf('copilot_stop_request').at(-1).request_id;
        },
        ask(text) {
            page.$('copilot-input').value = text;
            page.$('copilot-send-btn').click();
            return sentOf('copilot_chat_message').at(-1).request_id;
        },
    };
}

// placeholder is not an attribute the fake DOM reflects; read the property.
const ready = { disabled: false, placeholder: 'Спросить Claude...' };
const starting = { disabled: true, placeholder: 'запускается…' };

test('offline chip - agent state online arrives - both chips show online', () => {
    const p = livePage();
    p.status({ status: 'agent_state', agent_state: 'online' });
    assert.equal(p.chip(), '🟢 online');
    assert.equal(p.text(p.$('copilot-panel-chip')), '🟢 online');
});

test('online chip - agent state of an unknown value arrives - chip stays online', () => {
    const p = livePage();
    p.status({ status: 'agent_state', agent_state: 'online' });
    p.status({ status: 'agent_state', agent_state: 'sleeping' });
    assert.equal(p.chip(), '🟢 online');
});

test('spawn clicked - chip leaves offline - input is usable again', () => {
    const p = livePage();
    p.spawn();
    assert.deepEqual(p.input(), starting);
    p.status({ status: 'agent_state', agent_state: 'online' });
    assert.deepEqual(p.input(), ready);
});

test('stop clicked - chip reaches offline - stop button is back and disabled', () => {
    const p = livePage();
    p.status({ status: 'agent_state', agent_state: 'online' });
    p.stop();
    assert.deepEqual(p.stopButton(), { text: 'останавливается…', disabled: true });
    p.status({ status: 'agent_state', agent_state: 'offline' });
    assert.deepEqual(p.stopButton(), { text: 'Stop', disabled: true });
});

test('panel with history - channel connected arrives - nothing is added', () => {
    const p = livePage();
    p.status({ status: 'channel_connected' });
    assert.deepEqual(p.bubbles(), []);
});

test('spawn clicked - conference active answers it - spawn released and the reason shown', () => {
    const p = livePage();
    const spawn = p.spawn();
    p.status({ status: 'conference_active', request_id: spawn });
    assert.deepEqual(p.input(), ready);
    assert.deepEqual(p.bubbles(), [{ kind: 'status', text: 'занято конференцией' }]);
});

test('spawn clicked - conference active answers another tab - this spawn stays pending', () => {
    const p = livePage();
    p.spawn();
    p.status({ status: 'conference_active', request_id: 'other-tab' });
    assert.deepEqual(p.input(), starting);
    assert.deepEqual(p.bubbles(), []);
});

test('no request - conference active without id - the reason is shown', () => {
    const p = livePage();
    p.status({ status: 'conference_active' });
    assert.deepEqual(p.bubbles(), [{ kind: 'status', text: 'занято конференцией' }]);
});

test('manual session - preempted by a conference - the switch is shown', () => {
    const p = livePage();
    p.status({ status: 'manual_preempted' });
    assert.deepEqual(p.bubbles(), [{ kind: 'status', text: 'сессия переключена на конференцию' }]);
});

test('spawn clicked - bridge disconnected answers it - spawn can be retried', () => {
    const p = livePage();
    const spawn = p.spawn();
    p.status({ status: 'bridge_disconnected', request_id: spawn });
    assert.deepEqual(p.input(), ready);
    assert.deepEqual(p.bubbles(), [{ kind: 'error', text: 'Bridge offline: запрос не доставлен.' }]);
    const again = p.spawn();
    assert.notEqual(again, spawn);
});

test('stop clicked - supervisor disconnected answers it - stop can be retried', () => {
    const p = livePage();
    p.status({ status: 'agent_state', agent_state: 'online' });
    const stop = p.stop();
    p.status({ status: 'supervisor_disconnected', request_id: stop });
    assert.deepEqual(p.stopButton(), { text: 'Stop', disabled: false });
    assert.deepEqual(p.bubbles(), [{ kind: 'error', text: 'Supervisor offline: запрос не доставлен.' }]);
});

test('question asked - bridge disconnected answers it - its placeholder turns into the error', () => {
    const p = livePage();
    p.status({ status: 'agent_state', agent_state: 'online' });
    const question = p.ask('что решили');
    p.status({ status: 'bridge_disconnected', request_id: question });
    assert.deepEqual(p.bubbles(), [
        { kind: 'user', text: 'что решили' },
        { kind: 'error', text: 'Bridge offline: сообщение не доставлено.' },
    ]);
    // Resolved: a later status of the same request finds no placeholder.
    p.status({ status: 'working', request_id: question });
    assert.equal(p.bubbles()[1].text, 'Bridge offline: сообщение не доставлено.');
});

test('another tab asked - supervisor disconnected for its request - nothing is shown here', () => {
    const p = livePage();
    p.status({ status: 'supervisor_disconnected', request_id: 'other-tab' });
    assert.deepEqual(p.bubbles(), []);
});

test('no request - bridge disconnected without id - the error is shown', () => {
    const p = livePage();
    p.status({ status: 'bridge_disconnected' });
    assert.deepEqual(p.bubbles(), [{ kind: 'error', text: 'Bridge offline: запрос не доставлен.' }]);
});

const CHANNEL_OFFLINE = 'Channel offline: Claude не подключён.';

test('spawn clicked - channel disconnected answers it - spawn released with the error', () => {
    const p = livePage();
    const spawn = p.spawn();
    p.status({ status: 'channel_disconnected', request_id: spawn });
    assert.deepEqual(p.input(), ready);
    assert.deepEqual(p.bubbles(), [{ kind: 'error', text: CHANNEL_OFFLINE }]);
});

test('stop clicked - channel disconnected answers it - stop released with the error', () => {
    const p = livePage();
    p.status({ status: 'agent_state', agent_state: 'online' });
    const stop = p.stop();
    p.status({ status: 'channel_disconnected', request_id: stop });
    assert.deepEqual(p.stopButton(), { text: 'Stop', disabled: false });
    assert.deepEqual(p.bubbles(), [{ kind: 'error', text: CHANNEL_OFFLINE }]);
});

test('question asked - channel disconnected answers it - its placeholder shows the error', () => {
    const p = livePage();
    p.status({ status: 'agent_state', agent_state: 'online' });
    const question = p.ask('кто следующий');
    p.status({ status: 'channel_disconnected', request_id: question });
    assert.deepEqual(p.bubbles().map(b => b.kind), ['user', 'error']);
    assert.equal(p.bubbles()[1].text, CHANNEL_OFFLINE);
});

test('another tab asked - channel disconnected for its request - nothing is shown here', () => {
    const p = livePage();
    p.status({ status: 'agent_state', agent_state: 'online' });
    p.ask('кто следующий');
    p.status({ status: 'channel_disconnected', request_id: 'other-tab' });
    assert.deepEqual(p.bubbles().map(b => b.kind), ['user', 'assistant']);
});

test('two questions in flight - channel disconnected without id - both placeholders show the error', () => {
    const p = livePage();
    p.status({ status: 'agent_state', agent_state: 'online' });
    p.ask('первый');
    p.ask('второй');
    p.status({ status: 'channel_disconnected' });
    assert.deepEqual(p.bubbles(), [
        { kind: 'user', text: 'первый' },
        { kind: 'error', text: CHANNEL_OFFLINE },
        { kind: 'user', text: 'второй' },
        { kind: 'error', text: CHANNEL_OFFLINE },
    ]);
    p.status({ status: 'channel_disconnected' });
    assert.equal(p.bubbles().length, 5);
});

test('nothing in flight - channel disconnected without id - one error bubble', () => {
    const p = livePage();
    p.status({ status: 'channel_disconnected' });
    assert.deepEqual(p.bubbles(), [{ kind: 'error', text: CHANNEL_OFFLINE }]);
});

test('spawn clicked - error answers it - spawn released with the message', () => {
    const p = livePage();
    const spawn = p.spawn();
    p.status({ status: 'error', request_id: spawn, message: 'нет claude' });
    assert.deepEqual(p.input(), ready);
    assert.deepEqual(p.bubbles(), [{ kind: 'error', text: 'нет claude' }]);
});

test('stop clicked - error answers it - stop released with the message', () => {
    const p = livePage();
    p.status({ status: 'agent_state', agent_state: 'online' });
    const stop = p.stop();
    p.status({ status: 'error', request_id: stop, message: 'tmux упал' });
    assert.deepEqual(p.stopButton(), { text: 'Stop', disabled: false });
    assert.deepEqual(p.bubbles(), [{ kind: 'error', text: 'tmux упал' }]);
});

test('question asked - error without a message answers it - placeholder shows the default', () => {
    const p = livePage();
    p.status({ status: 'agent_state', agent_state: 'online' });
    const question = p.ask('итоги');
    p.status({ status: 'error', request_id: question });
    assert.deepEqual(p.bubbles()[1], { kind: 'error', text: 'Claude не смог ответить.' });
});

test('another tab asked - error for its request - shown as a new error bubble', () => {
    const p = livePage();
    p.status({ status: 'error', request_id: 'other-tab', message: 'сбой' });
    assert.deepEqual(p.bubbles(), [{ kind: 'error', text: 'сбой' }]);
});

test('question asked - done without a reply - placeholder says it ended without an answer', () => {
    const p = livePage();
    p.status({ status: 'agent_state', agent_state: 'online' });
    const question = p.ask('итоги');
    p.status({ status: 'done', request_id: question });
    assert.deepEqual(p.bubbles()[1], { kind: 'status', text: 'Запрос завершён без ответа.' });
});

test('question asked - done with a message - placeholder shows the message', () => {
    const p = livePage();
    p.status({ status: 'agent_state', agent_state: 'online' });
    const question = p.ask('итоги');
    p.status({ status: 'done', request_id: question, message: 'отменено' });
    assert.deepEqual(p.bubbles()[1], { kind: 'status', text: 'отменено' });
});

test('reply already landed - done arrives - nothing changes', () => {
    const p = livePage();
    p.status({ status: 'done', request_id: 'answered' });
    assert.deepEqual(p.bubbles(), []);
});

test('question asked - working then queued arrive - placeholder follows the progress', () => {
    const p = livePage();
    p.status({ status: 'agent_state', agent_state: 'online' });
    const question = p.ask('итоги');
    p.status({ status: 'working', request_id: question });
    const placeholder = () => p.document.querySelectorAll('#copilot-messages .copilot-msg-text')[1];
    assert.equal(p.text(placeholder()), 'Claude думает…');
    assert.equal(placeholder().className, 'copilot-msg-text pending');
    p.status({ status: 'queued', request_id: question });
    assert.equal(p.text(placeholder()), 'Status: queued');
});

test('no question - progress of an unknown request or a frame without status - nothing is shown', () => {
    const p = livePage();
    p.status({ status: 'working', request_id: 'other-tab' });
    p.socket().receive({ type: 'copilot_status' });
    assert.deepEqual(p.bubbles(), []);
});
