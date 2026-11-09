const { test } = require('node:test');
const assert = require('node:assert/strict');
const { loadPage, makeEvent } = require('./web_page.cjs');

const MEETING = { id: 'm1', title: 'Планёрка', source: 'orphan', created_at: '2026-11-09T10:00:00Z' };

function livePage() {
    const page = loadPage({ hash: '#/live', ready: true });
    const socket = page.socket();
    socket.open();
    const sentOf = type => socket.sent.filter(frame => frame.type === type);
    const messages = () => page.document.querySelectorAll('#copilot-messages .copilot-msg');
    return {
        ...page,
        socket,
        sentOf,
        messages,
        receive: frame => socket.receive(frame),
        online: () => socket.receive({ type: 'copilot_status', status: 'agent_state', agent_state: 'online' }),
        bubbles: () => messages().map(el => ({
            className: el.className,
            role: page.text(el.querySelector('.copilot-msg-role')),
            text: page.text(el.querySelector('.copilot-msg-text')),
        })),
        type(text) {
            page.$('copilot-input').value = text;
        },
        key(init) {
            const event = makeEvent('keydown', init);
            page.$('copilot-input').dispatchEvent(event);
            return event;
        },
        ask(text) {
            page.$('copilot-input').value = text;
            page.$('copilot-send-btn').click();
            return sentOf('copilot_chat_message').at(-1).request_id;
        },
    };
}

test('question typed - Enter pressed - sent and the input cleared', () => {
    const p = livePage();
    p.online();
    p.type('итоги?');
    const event = p.key({ key: 'Enter' });
    assert.equal(event.defaultPrevented, true);
    assert.equal(p.sentOf('copilot_chat_message')[0].text, 'итоги?');
    assert.equal(p.$('copilot-input').value, '');
});

test('question typed - Shift+Enter, another key or Enter while composing - nothing is sent', () => {
    const p = livePage();
    p.online();
    p.type('строка');
    assert.equal(p.key({ key: 'Enter', shiftKey: true }).defaultPrevented, false);
    p.key({ key: 'a' });
    p.key({ key: 'Enter', isComposing: true });
    assert.deepEqual(p.sentOf('copilot_chat_message'), []);
    assert.equal(p.$('copilot-input').value, 'строка');
});

test('long live transcript - question asked - the frame carries the last 15 speech rows', () => {
    const p = livePage();
    p.online();
    const rows = Array.from({ length: 20 }, (_, i) => ({
        id: i + 1, speaker: `S${i}`, text: `t${i}`, ts_display: `00:${i}`, speaker_id: `s${i}`,
    }));
    rows.splice(18, 0, { id: 99, speaker: '', text: 'Встреча', event_type: 'meeting_start', ts_display: '00:99' });
    rows.push({});
    p.receive({ type: 'init', meeting: MEETING, segments: rows });
    p.ask('что сейчас');
    const frame = p.sentOf('copilot_chat_message')[0];
    assert.equal(frame.meeting_id, 'm1');
    assert.equal(frame.meeting_title, 'Планёрка');
    assert.equal(frame.transcript_window.length, 15);
    assert.deepEqual(frame.transcript_window[0], { speaker: 'S6', text: 't6', timestamp: '00:6', segment_id: 7 });
    assert.deepEqual(frame.transcript_window.at(-1), { speaker: '', text: '', timestamp: '', segment_id: 0 });
    assert.equal(frame.transcript_window.some(row => row.segment_id === 99), false);
});

test('no meeting - question asked - the frame names no meeting', () => {
    const p = livePage();
    p.online();
    p.ask('привет');
    const frame = p.sentOf('copilot_chat_message')[0];
    assert.equal(frame.meeting_id, null);
    assert.equal(frame.meeting_title, null);
    assert.deepEqual(frame.transcript_window, []);
});

test('question in flight - reply arrives - bubble switches from the plain placeholder to rendered Markdown', () => {
    const p = livePage();
    p.online();
    const question = p.ask('**жирный** вопрос');
    assert.deepEqual(p.bubbles(), [
        { className: 'copilot-msg copilot-msg-user', role: 'Вы', text: '**жирный** вопрос' },
        { className: 'copilot-msg copilot-msg-assistant', role: 'Claude', text: 'Жду ответа Claude…' },
    ]);
    assert.equal(p.messages()[1].querySelector('.copilot-msg-text').className, 'copilot-msg-text pending');
    p.receive({ type: 'copilot_reply', request_id: question, kind: 'assistant', text: '**Да**', created_at: 0 });
    const textEl = p.messages()[1].querySelector('.copilot-msg-text');
    assert.equal(textEl.className, 'copilot-msg-text ai-answer');
    assert.equal(textEl.innerHTML, '<strong>Да</strong>');
    // The user's own text is never rendered as Markdown.
    assert.equal(p.messages()[0].querySelector('.copilot-msg-text').innerHTML, '**жирный** вопрос');
});

test('question in flight - progress with an empty text - placeholder shows an ellipsis', () => {
    const p = livePage();
    p.online();
    p.ask('вопрос');
    const assistantId = p.messages()[1].dataset.copilotId;
    p.evaluate(`updateCopilotMessage(${JSON.stringify(assistantId)}, { text: '' })`);
    assert.equal(p.bubbles()[1].text, '…');
});

test('proactive note arrives - shown as rendered Markdown with its own role and the unread dot', () => {
    const p = livePage();
    p.receive({ type: 'copilot_reply', kind: 'proactive', text: '- пункт', created_at: 0 });
    assert.deepEqual(p.bubbles(), [{ className: 'copilot-msg copilot-msg-proactive', role: 'Claude · proactive', text: 'пункт' }]);
    assert.equal(p.messages()[0].querySelector('.copilot-msg-text').innerHTML, '<ul><li>пункт</li></ul>');
    assert.equal(p.$('copilot-unread-dot').hidden, false);
});

test('question in flight - failure turns it into an error - role, class and time change in place', () => {
    const p = livePage();
    p.online();
    const question = p.ask('вопрос');
    const placeholder = p.messages()[1];
    const before = p.text(placeholder.querySelector('.copilot-msg-time'));
    p.receive({ type: 'copilot_status', status: 'error', request_id: question, message: 'сбой' });
    assert.equal(p.messages()[1], placeholder);
    assert.deepEqual(p.bubbles()[1], { className: 'copilot-msg copilot-msg-error', role: 'Error', text: 'сбой' });
    assert.match(p.text(placeholder.querySelector('.copilot-msg-time')), /^\d\d:\d\d:\d\d$/);
    assert.equal(typeof before, 'string');
});

test('no such bubble - an update for an unknown id - nothing changes', () => {
    const p = livePage();
    p.evaluate("updateCopilotMessage('missing', { text: 'x' })");
    assert.deepEqual(p.bubbles(), []);
});

test('question and stop in flight - the stream closes - both are reported and Stop is released', () => {
    const p = livePage();
    p.online();
    p.ask('вопрос');
    p.$('copilot-stop-btn').click();
    p.socket.close();
    assert.deepEqual(p.bubbles().map(b => [b.role, b.text]), [
        ['Вы', 'вопрос'],
        ['Error', 'Bridge offline: соединение прервано.'],
        ['Error', 'Bridge offline: результат остановки неизвестен.'],
    ]);
    assert.equal(p.text(p.$('copilot-stop-btn')), 'Stop');
});

test('spawn in flight - the stream closes - reported and the input is released', () => {
    const p = livePage();
    p.$('copilot-btn').click();
    assert.equal(p.$('copilot-input').disabled, true);
    p.socket.close();
    assert.deepEqual(p.bubbles().map(b => [b.role, b.text]), [
        ['Error', 'Bridge offline: результат запуска неизвестен.'],
    ]);
    assert.equal(p.$('copilot-input').disabled, false);
});

test('nothing in flight - the stream closes - no bubble is added', () => {
    const p = livePage();
    p.socket.close();
    assert.deepEqual(p.bubbles(), []);
});

test('conference meeting - controls updated - Stop is disabled and says why', () => {
    const p = livePage();
    p.online();
    p.receive({ type: 'init', meeting: { ...MEETING, source: 'ktalk' }, segments: [] });
    assert.equal(p.$('copilot-stop-btn').disabled, true);
    assert.equal(p.$('copilot-stop-btn').title, 'Conference Copilot sessions are managed automatically');
    p.receive({ type: 'meeting_change', meeting: MEETING });
    assert.equal(p.$('copilot-stop-btn').disabled, false);
    assert.equal(p.$('copilot-stop-btn').title, '');
});

test('session spawning - controls updated - input waits for the start', () => {
    const p = livePage();
    p.receive({ type: 'copilot_status', status: 'agent_state', agent_state: 'spawning' });
    assert.equal(p.$('copilot-input').disabled, true);
    assert.equal(p.$('copilot-input').placeholder, 'запускается…');
    assert.equal(p.$('copilot-send-btn').disabled, true);
});

test('stop sent - no offline within 15 s - reported as lost and Stop is usable again', () => {
    const p = livePage();
    p.online();
    p.$('copilot-stop-btn').click();
    p.timers.advance(15000);
    assert.deepEqual(p.bubbles().map(b => b.text), ['Stop не подтверждён за 15 с — запрос, похоже, потерялся. Попробуйте ещё раз.']);
    assert.equal(p.$('copilot-stop-btn').disabled, false);
});

test('stop sent - offline arrives in time - the timeout reports nothing', () => {
    const p = livePage();
    p.online();
    p.$('copilot-stop-btn').click();
    p.receive({ type: 'copilot_status', status: 'agent_state', agent_state: 'offline' });
    p.timers.advance(15000);
    assert.deepEqual(p.bubbles(), []);
});

test('stream closed - stop clicked - offline error and Stop stays usable', () => {
    const p = livePage();
    p.online();
    p.socket.close();
    p.$('copilot-stop-btn').click();
    assert.deepEqual(p.bubbles().map(b => b.text), ['Bridge offline: сообщение не отправлено.']);
    assert.equal(p.text(p.$('copilot-stop-btn')), 'Stop');
});

test('stream closed - spawn clicked - offline error and the input stays usable', () => {
    const p = livePage();
    p.socket.close();
    p.$('copilot-btn').click();
    assert.deepEqual(p.bubbles().map(b => b.text), ['Bridge offline: сообщение не отправлено.']);
    assert.equal(p.$('copilot-input').disabled, false);
    assert.equal(p.$('copilot-panel').style.display, 'flex');
});
