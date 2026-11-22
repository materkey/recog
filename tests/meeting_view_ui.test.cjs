const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { loadPage, json } = require('./web_page.cjs');

const META = {
    id: 'm1', title: 'Планёрка', source: 'ktalk', created_at: '2026-11-09T10:00:00Z', ended_at: '2026-11-09T11:02:05Z',
    stats: { duration_seconds: 3725, segment_count: 4, words_count: 9 },
    participants: { a: { name: 'Анна' }, b: { name: 'Борис' } },
    audio_mic_path: 'audio_mic.wav', audio_system_path: 'audio_system.wav',
};
const SEGMENTS = [
    { speaker: 'Анна', speaker_id: 'a', text: 'Начнём', ts_display: '10:00', is_local: false, audio_offset_ms: 0 },
    { speaker: 'Анна', speaker_id: 'a', text: 'с итогов', ts_display: '10:01', cont: true, audio_offset_ms: 900 },
    { speaker: '', text: 'Борис подключился', ts_display: '10:02', event_type: 'participant_join' },
    { speaker: 'Борис', speaker_id: 'b', text: 'Готово', ts_display: '10:03', is_local: true },
];

function meetingPage({ meta = META, segments = SEGMENTS, routes = {}, local = {}, confirm, privacy = { enabled: false, configured: true } } = {}) {
    const fetch = async (url, options) => {
        const method = options.method || 'GET';
        const key = `${method} ${url}`;
        if (routes[key]) return routes[key](options);
        if (key === 'GET /api/meetings/m1') return meta ? json(meta) : json({}, { status: 404 });
        if (key === 'GET /api/meetings/m1/transcript') return json(segments);
        if (key === 'GET /api/privacy') return json(privacy);
        if (key === 'GET /api/meetings') return json([]);
        return json({}, { status: 404 });
    };
    const page = loadPage({ hash: '#/meeting/m1', ready: true, fetch, local, confirm });
    const groups = () => page.document.querySelectorAll('#transcript .transcript-segment');
    return {
        ...page,
        groups,
        shown: () => groups().map(group => page.text(group)),
        events: () => page.document.querySelectorAll('#transcript .transcript-event').map(el => page.text(el)),
        meta: () => page.text(page.$('meeting-meta')),
        playButton: index => groups()[index].querySelector('.btn-play-segment'),
        notifications: () => page.document.querySelectorAll('.notification').map(el => [el.className, el.textContent]),
        settle: async () => { for (let i = 0; i < 5; i++) await page.flush(); },
    };
}

test('stored meeting - opened - title, meta and grouped transcript are shown', async () => {
    const p = meetingPage();
    assert.equal(p.text(p.$('meeting-title')), 'Loading...');
    await p.settle();
    assert.equal(p.text(p.$('meeting-title')), 'Планёрка');
    assert.match(p.meta(), /Duration 1h 2m/);
    assert.match(p.meta(), /Source ktalk Segments 4 Words 9/);
    assert.match(p.meta(), /Participants: Анна, Борис/);
    assert.match(p.meta(), /Audio Tracks: 🎤 Mic 🔊 System/);
    assert.deepEqual(p.shown(), ['Анна 10:00 Начнём с итогов', 'Борис 10:03 Готово']);
    assert.deepEqual(p.events(), ['10:02 Борис подключился']);
    assert.equal(p.groups()[0].dataset.segmentIndices, '0,1');
    assert.equal(p.groups()[1].className, 'transcript-segment speaker-local');
    assert.notEqual(p.playButton(0), null);
    assert.equal(p.playButton(1), null);
    assert.equal(p.$('delete-meeting-btn').style.display, '');
    assert.equal(p.$('view-meeting').style.display, '');
    assert.equal(p.$('view-meetings').style.display, 'none');
});

test('meeting still recording - opened - delete is hidden and minutes or seconds are shown', async () => {
    const p = meetingPage({ meta: { ...META, ended_at: null, stats: { duration_seconds: 125 }, participants: null, audio_mic_path: null, audio_system_path: null } });
    await p.settle();
    assert.equal(p.$('delete-meeting-btn').style.display, 'none');
    assert.match(p.meta(), /Duration 2m 5s/);
    assert.doesNotMatch(p.meta(), /Participants|Audio Tracks/);
    const short = meetingPage({ meta: { ...META, stats: { duration_seconds: 42 } } });
    await short.settle();
    assert.match(short.meta(), /Duration 42s/);
});

test('meeting without stats or segments - opened - placeholders are shown', async () => {
    const p = meetingPage({ meta: { ...META, title: '', stats: null, source: '', callrec: { recorder: 'samsung' }, audio_deleted_at: 'x' }, segments: [] });
    await p.settle();
    assert.equal(p.text(p.$('meeting-title')), 'Untitled Meeting');
    assert.match(p.meta(), /Duration - .*Source unknown Segments 0 Words 0/);
    assert.match(p.meta(), /Записано: Samsung/);
    assert.equal(p.text(p.$('transcript')), 'No transcript segments');
});

test('unknown meeting - opened - the error is shown in the transcript', async () => {
    const p = meetingPage({ meta: null });
    await p.settle();
    assert.equal(p.text(p.$('transcript')), 'Error: Meeting not found');
});

test('transcript answers a body that is not JSON - opened - the parser error is shown as text', async () => {
    const unparsable = { ok: true, status: 200, json: async () => { throw new SyntaxError('Unexpected token \'<\', "<b>oops</b>" is not valid JSON'); } };
    const p = meetingPage({ routes: { 'GET /api/meetings/m1/transcript': () => unparsable } });
    await p.settle();
    assert.equal(p.text(p.$('transcript')), 'Error: Unexpected token \'<\', "<b>oops</b>" is not valid JSON');
    assert.equal(p.$('transcript').querySelector('b'), null);
});

test('transcript shown - searching for a word - only matching rows, highlighted; clearing shows all', async () => {
    const p = meetingPage();
    await p.settle();
    const search = p.$('transcript-search');
    search.value = 'ИТОГ';
    search.dispatchEvent({ type: 'input', target: search, bubbles: true });
    assert.deepEqual(p.shown(), ['Анна 10:01 с итогов']);
    assert.equal(p.groups()[0].querySelector('mark').textContent, 'итог');
    search.value = 'борис';
    search.dispatchEvent({ type: 'input', target: search, bubbles: true });
    assert.deepEqual(p.shown(), ['Борис 10:03 Готово']);
    search.value = '';
    search.dispatchEvent({ type: 'input', target: search, bubbles: true });
    assert.equal(p.shown().length, 2);
});

const audioRoutes = (extra = {}) => ({
    'GET /api/meetings/m1/audio/0': async () => ({ ok: true, blob: async () => 'wav-0' }),
    'GET /api/meetings/m1/audio/1': async () => ({ ok: true, blob: async () => 'wav-1' }),
    ...extra,
});

test('segment with audio - play clicked - it plays and its row is highlighted', async () => {
    const p = meetingPage({ routes: audioRoutes() });
    await p.settle();
    const button = p.playButton(0);
    button.click();
    assert.equal(button.disabled, true);
    assert.equal(button.classList.contains('loading'), true);
    await p.settle();
    const audio = p.audios.at(-1);
    assert.equal(audio.playCalls, 1);
    assert.equal(button.className, 'btn-play-segment playing');
    assert.notEqual(button.querySelector('.stop-icon'), null);
    assert.equal(p.groups()[0].classList.contains('segment-playing'), true);
});

test('segment playing - play clicked again - playback stops and the row returns to normal', async () => {
    const p = meetingPage({ routes: audioRoutes() });
    await p.settle();
    const button = p.playButton(0);
    button.click();
    await p.settle();
    button.click();
    assert.equal(p.audios.at(-1).paused, true);
    assert.equal(button.className, 'btn-play-segment');
    assert.notEqual(button.querySelector('.play-icon'), null);
    assert.equal(p.groups()[0].classList.contains('segment-playing'), false);
});

test('segment playing - audio ends - the button returns to play', async () => {
    const p = meetingPage({ routes: audioRoutes() });
    await p.settle();
    p.playButton(0).click();
    await p.settle();
    p.audios.at(-1).emit('ended');
    assert.equal(p.playButton(0).className, 'btn-play-segment');
});

test('segment playing - playback fails - stopped and an error is shown', async () => {
    const p = meetingPage({ routes: audioRoutes() });
    await p.settle();
    p.playButton(0).click();
    await p.settle();
    p.audios.at(-1).emit('error');
    assert.equal(p.playButton(0).className, 'btn-play-segment');
    assert.deepEqual(p.notifications(), [['notification notification-error', 'Audio playback failed']]);
    p.timers.advance(3300);
    assert.deepEqual(p.notifications(), []);
});

test('segment loading - play clicked again before it loads - only one request is made', async () => {
    const p = meetingPage({ routes: audioRoutes() });
    await p.settle();
    p.playButton(0).click();
    p.playButton(0).click();
    await p.settle();
    assert.equal(p.requests.filter(r => r.url.includes('/audio/')).length, 1);
});

test('audio deleted on the server - play clicked - the server reason is shown and the button reset', async () => {
    const p = meetingPage({
        routes: { 'GET /api/meetings/m1/audio/0': async () => json({ detail: 'No audio file' }, { status: 404 }) },
    });
    await p.settle();
    p.playButton(0).click();
    await p.settle();
    assert.equal(p.playButton(0).disabled, false);
    assert.equal(p.playButton(0).className, 'btn-play-segment');
    assert.deepEqual(p.notifications(), [['notification notification-error', 'Audio error: No audio file']]);
});

test('audio endpoint fails without a reason - play clicked - a generic error is shown', async () => {
    const p = meetingPage({
        routes: { 'GET /api/meetings/m1/audio/0': async () => ({ ok: false, json: async () => { throw new Error('html'); } }) },
    });
    await p.settle();
    p.playButton(0).click();
    await p.settle();
    assert.deepEqual(p.notifications(), [['notification notification-error', 'Audio error: Failed to load audio']]);
});

const meetingWithTwoPlayable = SEGMENTS.map((seg, i) => (i === 3 ? { ...seg, audio_offset_ms: 3000 } : seg));

test('one segment playing - another segment played - the first stops before the second starts', async () => {
    const p = meetingPage({ segments: meetingWithTwoPlayable, routes: { ...audioRoutes(), 'GET /api/meetings/m1/audio/3': async () => ({ ok: true, blob: async () => 'wav-3' }) } });
    await p.settle();
    p.playButton(0).click();
    await p.settle();
    p.playButton(1).click();
    assert.equal(p.audios[0].paused, true);
    assert.equal(p.groups()[0].classList.contains('segment-playing'), false);
    await p.settle();
    assert.equal(p.groups()[1].classList.contains('segment-playing'), true);
});

test('segment loading - another segment clicked - the first load is abandoned', async () => {
    let release;
    const slow = new Promise(resolve => { release = resolve; });
    const p = meetingPage({
        segments: meetingWithTwoPlayable,
        routes: {
            'GET /api/meetings/m1/audio/0': async () => { await slow; return { ok: true, blob: async () => 'wav-0' }; },
            'GET /api/meetings/m1/audio/3': async () => ({ ok: true, blob: async () => 'wav-3' }),
        },
    });
    await p.settle();
    p.playButton(0).click();
    await p.flush();
    p.playButton(1).click();
    await p.settle();
    release();
    await p.settle();
    assert.equal(p.audios.length, 1);
    assert.equal(p.audios[0].src.startsWith('blob:'), true);
    assert.equal(p.groups()[1].classList.contains('segment-playing'), true);
    assert.equal(p.groups()[0].classList.contains('segment-playing'), false);
});

test('segment loading - another segment takes over playback - the first button returns to play at once', async () => {
    let release;
    const slow = new Promise(resolve => { release = resolve; });
    const p = meetingPage({
        segments: meetingWithTwoPlayable,
        routes: {
            'GET /api/meetings/m1/audio/0': async () => { await slow; return { ok: true, blob: async () => 'wav-0' }; },
            'GET /api/meetings/m1/audio/3': async () => ({ ok: true, blob: async () => 'wav-3' }),
        },
    });
    await p.settle();
    p.playButton(0).click();
    await p.flush();
    p.playButton(1).click();
    assert.equal(p.playButton(0).disabled, false);
    assert.equal(p.playButton(0).className, 'btn-play-segment');
    await p.settle();
    assert.equal(p.playButton(1).className, 'btn-play-segment playing');
    release();
    await p.settle();
    assert.equal(p.playButton(0).disabled, false);
    assert.equal(p.playButton(0).className, 'btn-play-segment');
    assert.notEqual(p.playButton(0).querySelector('.play-icon'), null);
});

test('segment loading - its load fails after another segment took over - the new playback goes on, no error shown', async () => {
    let fail;
    const slow = new Promise((resolve, reject) => { fail = reject; });
    const p = meetingPage({
        segments: meetingWithTwoPlayable,
        routes: {
            'GET /api/meetings/m1/audio/0': async () => { await slow; return { ok: true, blob: async () => 'wav-0' }; },
            'GET /api/meetings/m1/audio/3': async () => ({ ok: true, blob: async () => 'wav-3' }),
        },
    });
    await p.settle();
    p.playButton(0).click();
    await p.flush();
    p.playButton(1).click();
    await p.settle();
    fail(new Error('network down'));
    await p.settle();
    assert.equal(p.playButton(1).className, 'btn-play-segment playing');
    assert.equal(p.playButton(0).className, 'btn-play-segment');
    assert.equal(p.playButton(0).disabled, false);
    assert.deepEqual(p.notifications(), []);
});

test('meeting shown - Copy clicked - Markdown with real names is copied and the button confirms', async () => {
    const p = meetingPage();
    await p.settle();
    const copy = p.document.querySelector('.meeting-actions .btn');
    copy.click();
    await p.settle();
    assert.equal(p.clipboard.length, 1);
    assert.match(p.clipboard[0], /^# Планёрка\n\n\*\*Date:\*\* 9 ноября 2026 г\.\n\n---\n\n/);
    assert.match(p.clipboard[0], /\*\*Анна\*\* \*\[10:00\]\*: Начнём с итогов\n\n\*\[10:02\] Борис подключился\*\n\n\*\*Борис\*\* \*\[10:03\]\*: Готово\n\n$/);
    assert.equal(copy.textContent, 'Copied!');
    p.timers.advance(1500);
    assert.equal(copy.textContent, 'Copy');
});

test('privacy on - Copy clicked - speakers are replaced by aliases in order of appearance', async () => {
    const p = meetingPage({
        privacy: { enabled: true, configured: true },
        segments: [...SEGMENTS, { speaker: null, text: 'кто это', ts_display: '10:04' }],
    });
    await p.settle();
    p.document.querySelector('.meeting-actions .btn').click();
    await p.settle();
    assert.match(p.clipboard[0], /\*\*speaker_0\*\* \*\[10:00\]\*: Начнём с итогов/);
    assert.match(p.clipboard[0], /\*\*speaker_1\*\* \*\[10:03\]\*: Готово/);
    assert.match(p.clipboard[0], /\*\*speaker_\?\*\* \*\[10:04\]\*: кто это/);
    assert.doesNotMatch(p.clipboard[0], /\*\*Анна|\*\*Борис/);
});

test('clipboard refused - Copy clicked - the button is unchanged', async () => {
    const p = meetingPage();
    p.context.navigator.clipboard.writeText = () => Promise.reject(new Error('denied'));
    await p.settle();
    const copy = p.document.querySelector('.meeting-actions .btn');
    copy.click();
    await p.settle();
    assert.equal(copy.textContent, 'Copy');
    assert.equal(p.errors.length, 1);
});

test('meeting shown - Export MD clicked - a Markdown file named after the meeting is downloaded', async () => {
    const p = meetingPage();
    await p.settle();
    p.document.querySelectorAll('.meeting-actions .btn')[1].click();
    assert.equal(p.downloads.length, 1);
    assert.equal(p.downloads[0].name, 'm1.md');
    assert.equal(p.downloads[0].blob.type, 'text/markdown');
    assert.match(p.downloads[0].blob.parts[0], /\*\*Борис\*\* \*\[10:03\]\*: Готово/);
});

test('meeting not loaded - Copy or Export clicked - nothing happens', () => {
    const p = meetingPage();
    p.evaluate('copyToClipboard(); exportMarkdown()');
    assert.deepEqual(p.downloads, []);
    assert.deepEqual(p.clipboard, []);
});

const orphan = { ...META, title: null, source: 'orphan' };

test('orphan without a title - Generate Title clicked - the generated title replaces the button', async () => {
    const p = meetingPage({
        meta: orphan,
        routes: { 'POST /api/meetings/m1/generate-title': async () => json({ title: 'Созвон про релиз', title_source: 'ai' }) },
    });
    await p.settle();
    const button = p.document.querySelector('.btn-generate-title');
    button.click();
    assert.equal(button.textContent, 'Generating...');
    assert.equal(button.disabled, true);
    await p.settle();
    assert.equal(p.text(p.$('meeting-title')), 'Созвон про релиз');
    assert.equal(p.document.querySelector('.btn-generate-title'), null);
});

test('orphan without a title - generation fails - the reason shows, then the button comes back', async () => {
    const p = meetingPage({
        meta: orphan,
        routes: { 'POST /api/meetings/m1/generate-title': async () => json({ detail: 'Codex недоступен' }, { status: 503 }) },
    });
    await p.settle();
    const button = p.document.querySelector('.btn-generate-title');
    button.click();
    await p.settle();
    assert.equal(p.text(button), 'Error: Codex недоступен');
    p.timers.advance(3000);
    assert.equal(p.text(button), 'Generate Title (Codex)');
    assert.equal(button.disabled, false);
});

test('generation fails without a reason - the generic error is shown', async () => {
    const p = meetingPage({
        meta: orphan,
        routes: { 'POST /api/meetings/m1/generate-title': async () => ({ ok: false, json: async () => { throw new Error('html'); } }) },
    });
    await p.settle();
    p.document.querySelector('.btn-generate-title').click();
    await p.settle();
    assert.equal(p.text(p.document.querySelector('.btn-generate-title')), 'Error: Failed to generate title');
});

test('meeting shown - Delete declined - nothing is deleted', async () => {
    const p = meetingPage({ confirm: () => false });
    await p.settle();
    p.$('delete-meeting-btn').click();
    await p.settle();
    assert.deepEqual(p.confirms, ['Удалить митинг «Планёрка»? Действие необратимо.']);
    assert.equal(p.requests.some(r => r.method === 'DELETE'), false);
});

test('meeting shown - Delete confirmed - deleted, reported and back to the list', async () => {
    const p = meetingPage({ routes: { 'DELETE /api/meetings/m1': async () => json({ deleted: true }) } });
    await p.settle();
    p.$('delete-meeting-btn').click();
    assert.equal(p.text(p.$('delete-meeting-btn')), 'Deleting...');
    await p.settle();
    assert.deepEqual(p.notifications(), [['notification notification-success', 'Митинг удалён']]);
    assert.equal(p.window.location.hash, '/');
});

test('meeting shown - Delete fails - the reason is shown and Delete is usable again', async () => {
    const p = meetingPage({ routes: { 'DELETE /api/meetings/m1': async () => json({ detail: 'занято' }, { status: 409 }) } });
    await p.settle();
    p.$('delete-meeting-btn').click();
    await p.settle();
    assert.deepEqual(p.notifications(), [['notification notification-error', 'Ошибка удаления: занято']]);
    assert.equal(p.text(p.$('delete-meeting-btn')), 'Delete');
    assert.equal(p.$('delete-meeting-btn').disabled, false);
});

test('untitled meeting - Delete fails without a reason - the generic error names no title', async () => {
    const p = meetingPage({
        meta: { ...META, title: '' },
        routes: { 'DELETE /api/meetings/m1': async () => ({ ok: false, json: async () => { throw new Error('html'); } }) },
    });
    await p.settle();
    p.$('delete-meeting-btn').click();
    await p.settle();
    assert.deepEqual(p.confirms, ['Удалить митинг «Untitled»? Действие необратимо.']);
    assert.deepEqual(p.notifications(), [['notification notification-error', 'Ошибка удаления: Failed to delete meeting']]);
});

test('no meeting open - Generate Title or Delete called - nothing is requested', () => {
    const p = loadPage({ hash: '#/' });
    p.evaluate('generateTitle(); deleteMeeting()');
    assert.deepEqual(p.confirms, []);
    assert.deepEqual(p.requests, []);
});

// A fixed box pinned by both its top and its bottom stretches between them,
// and the toast covered the page from top to bottom.
test('meeting shown - a toast appears - its own style and the stylesheet pin one vertical edge', async () => {
    const p = meetingPage();
    await p.settle();
    p.evaluate("showNotification('Скопировано')");
    const css = fs.readFileSync(path.join(__dirname, '../crates/recog-web/static/style.css'), 'utf8');
    const rules = [...css.matchAll(/\.notification\s*\{([^}]*)\}/g)].map(rule => rule[1]);
    const declarations = [p.document.querySelector('.notification').style.cssText, ...rules].join(';');
    const pinned = ['top', 'bottom'].filter(edge => new RegExp(`(?:^|[\\s;])${edge}\\s*:`).test(declarations));
    assert.ok(rules.length > 0, 'the stylesheet has no toast rule');
    assert.deepEqual(pinned, ['top']);
});

test('meeting page - route changes to live and to the list - the nav link follows', async () => {
    const p = meetingPage();
    await p.settle();
    p.setHash('#/live');
    const active = () => p.document.querySelectorAll('.nav-link.active').map(link => link.getAttribute('href'));
    assert.deepEqual(active(), ['#/live']);
    assert.equal(p.$('view-live').style.display, '');
    p.setHash('#/');
    assert.deepEqual(active(), ['#/']);
    p.setHash('#/unknown');
    assert.deepEqual(active(), []);
    assert.equal(p.$('view-meetings').style.display, '');
});
