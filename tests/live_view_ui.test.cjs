const { test } = require('node:test');
const assert = require('node:assert/strict');
const { loadPage, json } = require('./web_page.cjs');

const MEETING = { id: 'm1', title: 'Планёрка', source: 'ktalk', created_at: '2026-11-09T10:00:00Z' };
const segment = (id, text, extra = {}) => ({
    id, speaker: 'Анна', speaker_id: 'a', text, ts_display: `10:0${id}`, is_local: false, ...extra,
});

function livePage({ local = {}, fetch } = {}) {
    const page = loadPage({ hash: '#/live', ready: true, local, fetch });
    const socket = page.socket();
    socket.open();
    const rows = () => page.document.querySelectorAll('#live-transcript .transcript-segment');
    return {
        ...page,
        socket,
        receive: frame => socket.receive(frame),
        init: (segments, extra = {}) => socket.receive({ type: 'init', meeting: MEETING, segments, ...extra }),
        rows,
        shown: () => rows().map(row => ({
            id: row.dataset.segmentId,
            text: page.text(row.querySelector('.segment-text')),
            refined: row.dataset.refined,
        })),
        info: () => page.text(page.$('live-meeting-info')),
        transcript: () => page.text(page.$('live-transcript')),
        copied: async () => {
            page.$('copy-live-btn').click();
            await page.flush();
            return page.clipboard.at(-1);
        },
    };
}

test('stream opened - init with segments - meeting card and every row are shown', () => {
    const p = livePage();
    assert.equal(p.text(p.$('live-status')), 'Connected');
    p.init([segment(1, 'привет'), segment(2, 'начнём', { is_local: true, speaker: 'Я' })]);
    assert.match(p.info(), /ktalk Планёрка/);
    assert.deepEqual(p.shown().map(row => row.text), ['привет', 'начнём']);
    assert.deepEqual(p.rows().map(row => row.className), [
        'transcript-segment speaker-other segment-new',
        'transcript-segment speaker-local segment-new',
    ]);
    p.timers.advance(500);
    assert.equal(p.rows()[0].className, 'transcript-segment speaker-other');
});

test('stream opened - init without a meeting or segments - waiting states are shown', () => {
    const p = livePage();
    p.receive({ type: 'init', meeting: null });
    assert.equal(p.info(), 'No active meeting');
    assert.equal(p.transcript(), 'Waiting for transcription...');
    p.receive({ type: 'segments', data: [segment(1, 'первое')] });
    assert.equal(p.transcript().includes('Waiting'), false);
    assert.deepEqual(p.shown().map(row => row.text), ['первое']);
});

test('meeting shown - meeting_change arrives - old rows go and the new meeting is shown', () => {
    const p = livePage();
    p.init([segment(1, 'старое')]);
    p.receive({ type: 'meeting_change', meeting: { ...MEETING, id: 'm2', title: 'Ретро' } });
    assert.match(p.info(), /Ретро/);
    assert.equal(p.transcript(), 'Waiting for transcription...');
});

test('refine enabled - tier-1 rows arrive - marked as preliminary until refined', () => {
    const p = livePage();
    p.init([], { refine_enabled: true });
    p.receive({ type: 'segments', data: [segment(1, 'черновик'), { ...segment(2, 'Встреча началась'), event_type: 'meeting_start' }] });
    const [row, event] = p.rows();
    assert.equal(row.dataset.refined, 'false');
    assert.equal(row.title, 'Предварительная расшифровка — будет уточнена');
    assert.equal(event.dataset.refined, undefined);
    p.receive({ type: 'transcript_refine', meeting_id: 'm1', updates: [{ segment_id: 1, text: 'чистовик' }] });
    assert.deepEqual(p.shown()[0], { id: '1', text: 'чистовик', refined: 'true' });
    assert.equal(p.rows()[0].title, '');
});

test('refine disabled - rows arrive - no preliminary cue unless already refined', () => {
    const p = livePage();
    p.init([segment(1, 'текст'), segment(2, 'уточнено', { refined: true })]);
    assert.deepEqual(p.shown().map(row => row.refined), [undefined, 'true']);
});

test('orphan meeting - refine_state turns refine on - new rows get the cue and the meeting counts as KTalk', () => {
    const p = livePage();
    p.init([segment(1, 'до')], { meeting: { ...MEETING, source: 'orphan' } });
    p.receive({ type: 'refine_state', meeting_id: 'other', refine_enabled: true });
    p.receive({ type: 'segments', data: [segment(2, 'другая встреча')] });
    assert.equal(p.rows()[1].dataset.refined, undefined);
    p.receive({ type: 'refine_state', meeting_id: 'm1', refine_enabled: true });
    p.receive({ type: 'segments', data: [segment(3, 'после')] });
    assert.deepEqual(p.shown().map(row => row.text), ['до', 'другая встреча', 'после']);
    assert.equal(p.rows()[0].dataset.refined, undefined);
    assert.equal(p.rows()[2].dataset.refined, 'false');
    assert.equal(p.evaluate('views.live.currentMeeting.source'), 'ktalk');
});

test('rows shown - refine of another meeting arrives - nothing changes', () => {
    const p = livePage();
    p.init([segment(1, 'текст')]);
    p.receive({ type: 'transcript_refine', meeting_id: 'm2', updates: [{ segment_id: 1, text: 'чужое' }] });
    assert.deepEqual(p.shown()[0], { id: '1', text: 'текст', refined: undefined });
});

test('rows shown - malformed refine updates arrive - only the valid one applies', async () => {
    const p = livePage();
    p.init([segment(1, 'один'), segment(2, 'два'), segment(3, 'три')]);
    p.receive({
        type: 'transcript_refine',
        updates: [null, 'text', { segment_id: 0, text: 'ноль' }, { segment_id: '2', text: 'строка' },
            { segment_id: 1, text: 5 }, { segment_id: 3, text: 'три!' }],
    });
    assert.deepEqual(p.shown().map(row => row.text), ['один', 'два', 'три!']);
    p.receive({ type: 'transcript_refine', updates: 'none' });
    // The copy is built from the stored segments, which took the refined text too.
    assert.match(await p.copied(), /\*\*Анна\*\* \*\[10:03\]\*: три!\n/);
});

test('refine arrives before its row - the row arrives - shown refined', () => {
    const p = livePage();
    p.receive({ type: 'transcript_refine', meeting_id: 'm1', refined_at: 1, updates: [{ segment_id: 4, text: 'уточнено' }] });
    p.init([segment(4, 'сырое')]);
    assert.deepEqual(p.shown(), [{ id: '4', text: 'уточнено', refined: 'true' }]);
    // Consumed: the same id in a later frame keeps its own text.
    p.receive({ type: 'segments', data: [segment(4, 'повтор')] });
    assert.equal(p.shown()[1].text, 'повтор');
});

test('refine pending for one meeting - init lands on another - the pending refine is dropped', () => {
    const p = livePage();
    p.init([]);
    p.receive({ type: 'transcript_refine', updates: [{ segment_id: 1, text: 'от первой' }] });
    p.receive({ type: 'init', meeting: { ...MEETING, id: 'm2' }, segments: [segment(1, 'вторая')] });
    assert.equal(p.shown()[0].text, 'вторая');
});

test('refine pending - init without a meeting - the pending refine is kept', () => {
    const p = livePage();
    p.init([]);
    p.receive({ type: 'transcript_refine', updates: [{ segment_id: 2, text: 'ждёт' }] });
    p.receive({ type: 'init', meeting: null, segments: [] });
    p.receive({ type: 'segments', data: [segment(2, 'сырое')] });
    assert.equal(p.shown()[0].text, 'ждёт');
});

test('row with audio - play clicked - the segment audio of the meeting is requested', async () => {
    const p = livePage({
        fetch: async url => (url.includes('/audio/') ? { ok: true, blob: async () => 'wav' } : json({})),
    });
    p.init([segment(1, 'без звука'), segment(2, 'со звуком', { audio_offset_ms: 1200 })]);
    assert.equal(p.rows()[0].querySelector('.btn-play-segment'), null);
    p.rows()[1].querySelector('.btn-play-segment').click();
    await p.flush();
    assert.equal(p.requests.at(-1).url, '/api/meetings/m1/audio/1');
    assert.equal(p.audios.at(-1).playCalls, 1);
});

test('follow mode on - rows arrive - the transcript scrolls to the bottom', () => {
    const p = livePage();
    p.$('live-transcript').scrollHeight = 900;
    p.init([segment(1, 'текст')]);
    assert.equal(p.$('live-transcript').scrollTop, 900);
});

test('follow mode on - follow clicked and rows arrive - no scrolling until it is back on', () => {
    const p = livePage();
    const transcript = p.$('live-transcript');
    transcript.scrollHeight = 900;
    p.$('follow-btn').click();
    assert.equal(p.$('follow-btn').classList.contains('active'), false);
    assert.equal(p.window.localStorage.getItem('liveFollowMode'), 'false');
    p.init([segment(1, 'текст')]);
    assert.equal(transcript.scrollTop, 0);
    transcript.scrollHeight = 1500;
    p.$('follow-btn').click();
    assert.equal(p.$('follow-btn').classList.contains('active'), true);
    assert.equal(transcript.scrollTop, 1500);
});

test('follow mode saved off - live view opens - button shows it off', () => {
    const p = livePage({ local: { liveFollowMode: 'false' } });
    assert.equal(p.$('follow-btn').classList.contains('active'), false);
});

test('stream open - error frame arrives - the message is shown in the meeting card', () => {
    const p = livePage();
    p.receive({ type: 'error', message: 'нет данных' });
    assert.equal(p.info(), 'нет данных');
});

test('stream open - error frame with markup arrives - the markup is shown as text', () => {
    const p = livePage();
    p.receive({ type: 'error', message: '<img src=x onerror=alert(1)> нет данных' });
    assert.equal(p.info(), '<img src=x onerror=alert(1)> нет данных');
    assert.equal(p.$('live-meeting-info').querySelector('img'), null);
});

test('privacy loading - privacy_updated arrives - both privacy buttons follow it', () => {
    const p = livePage();
    p.receive({ type: 'privacy_updated', enabled: true });
    assert.equal(p.$('live-privacy-btn').classList.contains('active'), true);
    assert.equal(p.$('live-privacy-btn').disabled, false);
    assert.equal(p.window.localStorage.getItem('meetingPrivacyMode'), 'true');
});

test('profile changed elsewhere - user_profile_status arrives - the profile is read again', async () => {
    const p = livePage();
    await p.flush();
    const before = p.requests.filter(r => r.url === '/api/user-profile').length;
    p.receive({ type: 'user_profile_status' });
    await p.flush();
    assert.equal(p.requests.filter(r => r.url === '/api/user-profile').length, before + 1);
});

test('stream open - connection drops or fails - status says so', () => {
    const p = livePage();
    p.socket.onerror();
    assert.equal(p.text(p.$('live-status')), 'Connection error');
    p.socket.close();
    assert.equal(p.text(p.$('live-status')), 'Disconnected');
});

test('rows shown - download clicked - a Markdown file named after the meeting', () => {
    const p = livePage();
    p.init([segment(1, 'привет')]);
    p.$('download-live-btn').click();
    assert.equal(p.downloads.length, 1);
    assert.equal(p.downloads[0].name, 'm1.md');
    assert.match(p.downloads[0].blob.parts[0], /^# Планёрка\n\n\*\*Date:\*\* .*\n\n---\n\n\*\*Анна\*\* \*\[10:01\]\*: привет\n\n$/);
});

test('rows without a meeting - download clicked - a dated live file with the default title', () => {
    const p = livePage();
    p.receive({ type: 'init', meeting: null, segments: [segment(1, 'привет')] });
    p.$('download-live-btn').click();
    assert.match(p.downloads[0].name, /^live-\d+\.md$/);
    assert.match(p.downloads[0].blob.parts[0], /^# Live Transcription\n/);
});

test('no rows - copy or download clicked - nothing is produced', async () => {
    const p = livePage();
    p.$('download-live-btn').click();
    p.$('copy-live-btn').click();
    await p.flush();
    assert.deepEqual(p.downloads, []);
    assert.deepEqual(p.clipboard, []);
});

test('rows shown - copy clicked - the button confirms and comes back', async () => {
    const p = livePage();
    p.init([segment(1, 'привет')]);
    await p.copied();
    assert.equal(p.text(p.$('copy-live-btn')), 'Copied!');
    p.timers.advance(1500);
    assert.equal(p.text(p.$('copy-live-btn')), 'Copy MD');
});
