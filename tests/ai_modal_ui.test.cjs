const { test } = require('node:test');
const assert = require('node:assert/strict');
const { loadPage, json } = require('./web_page.cjs');

const META = {
    id: 'm1', title: 'Планёрка', source: 'ktalk', created_at: '2026-11-09T10:00:00Z', ended_at: '2026-11-09T11:00:00Z',
    participants: {
        me: { name: 'Вера', is_local: true }, anna: { name: 'Анна' }, twin: { name: 'Анна' },
        boris: {}, __system__: { name: 'System' },
    },
};
const PROMPTS = [{ id: 'summary', name: 'Итоги' }, { id: 'my-increment', name: 'Мой инкремент' }];
const version = (v, extra = {}) => ({
    version: v, status: 'completed', generated_at: '2026-11-09T11:00:00Z', segment_count: 4,
    provider: 'codex', model: 'gpt-5.6', reasoning_effort: 'high', ...extra,
});

// The meeting page with its AI Analysis modal; `routes` answers by "METHOD path".
function aiPage({ increments = { summary: [version(1)] }, contents = {}, routes = {}, meta = META } = {}) {
    const fetch = async (url, options) => {
        const key = `${options.method || 'GET'} ${url}`;
        if (routes[key]) return routes[key](options);
        const found = Object.keys(routes).find(prefix => prefix.endsWith('*') && key.startsWith(prefix.slice(0, -1)));
        if (found) return routes[found](options, url);
        if (key === 'GET /api/meetings/m1') return json(meta);
        if (key === 'GET /api/meetings/m1/transcript') return json([]);
        if (key === 'GET /api/privacy') return json({ enabled: false, configured: true });
        if (key === 'GET /api/prompts') return json(PROMPTS);
        if (key === 'GET /api/meetings/m1/increments') return json(increments);
        const content = key.match(/^GET \/api\/meetings\/m1\/increments\/([^/]+)\/(\d+)$/);
        if (content) {
            const answer = contents[`${decodeURIComponent(content[1])}/${content[2]}`];
            if (answer === undefined) return json({ meta: version(Number(content[2])), content: `Ответ v${content[2]}` });
            return typeof answer === 'function' ? answer() : answer;
        }
        return json({}, { status: 404 });
    };
    const page = loadPage({ hash: '#/meeting/m1', ready: true, fetch });
    const settle = async () => { for (let i = 0; i < 8; i++) await page.flush(); };
    const savedView = () => page.$('ai-saved-view');
    return {
        ...page,
        settle,
        async open() {
            await settle();
            page.document.querySelector('.meeting-actions .btn[onclick="analyzeIncrement()"]').click();
            await settle();
        },
        content: () => page.words(page.$('increment-content')),
        contentEl: () => page.$('increment-content'),
        promptTabs: () => savedView().querySelectorAll('.modal-tabs .tab-btn').map(btn => [page.text(btn), btn.classList.contains('active')]),
        versionTabs: () => page.document.querySelectorAll('#version-tabs .version-tab-btn').map(btn => page.text(btn).replace(/ \d\d:\d\d/, '')),
        footerButton: label => savedView().querySelectorAll('.modal-footer .btn').find(btn => page.text(btn) === label),
        participants: () => page.$('increment-participant-select').querySelectorAll('option').map(option => [option.value, page.text(option)]),
        choose(participantId) {
            const select = page.$('increment-participant-select');
            select.value = participantId;
            select.dispatchEvent({ type: 'change', target: select, bubbles: true });
        },
        newContent: () => page.words(page.$('ai-new-content')),
        posts: () => page.requests.filter(r => r.method === 'POST').map(r => r.url),
    };
}

test('completed version with duration and participant - opened - model, details and answer without frontmatter', async () => {
    const p = aiPage({
        contents: { 'summary/1': json({ meta: version(1, { duration_seconds: 12.34, participant_name: 'Анна' }), content: '---\nid: x\n---\n## Решения\n- **релиз** в пятницу' }) },
    });
    await p.open();
    assert.deepEqual(p.promptTabs(), [['Итоги', true]]);
    assert.equal(p.text(p.contentEl().querySelector('.increment-model-badge')), 'Model: Codex · gpt-5.6 · high');
    assert.equal(p.text(p.contentEl().querySelector('.increment-result-details')), 'Итоги · Анна · v1 · 4 segments · 12.3s');
    assert.equal(p.contentEl().querySelector('.ai-answer').innerHTML, '<h3>Решения</h3><ul><li><strong>релиз</strong> в пятницу</li></ul>');
    p.footerButton('Copy').click();
    await p.settle();
    assert.equal(p.clipboard.at(-1), '## Решения\n- **релиз** в пятницу');
});

for (const [meta, badge] of [
    [{ provider: 'claude', model: null, reasoning_effort: null }, 'Model: Claude · Model unavailable'],
    [{ provider: 'gemini', model: 'g-2', reasoning_effort: null, effort: 'low' }, 'Model: gemini · g-2 · low'],
    [{ provider: null, model: null, reasoning_effort: null }, 'Model: Model unavailable'],
]) {
    test(`version by ${meta.provider} - opened - badge reads ${badge}`, async () => {
        const p = aiPage({ contents: { 'summary/1': json({ meta: version(1, meta), content: 'Текст без шапки' }) } });
        await p.open();
        assert.equal(p.text(p.contentEl().querySelector('.increment-model-badge')), badge);
        assert.equal(p.text(p.contentEl().querySelector('.increment-result-details')), 'Итоги · v1 · 4 segments');
    });
}

test('prompt unknown to the prompt list - opened - its id names the tab and the details', async () => {
    const p = aiPage({ increments: { custom: [version(1)] } });
    await p.open();
    assert.deepEqual(p.promptTabs(), [['custom', true]]);
    assert.match(p.text(p.contentEl().querySelector('.increment-result-details')), /^custom · v1/);
});

test('failed version - opened - the failure is shown with its reason or a default', async () => {
    const p = aiPage({
        increments: { summary: [version(1, { status: 'failed', error: 'таймаут' }), version(2, { status: 'failed' })] },
        contents: {
            'summary/1': json({ meta: version(1, { status: 'failed', error: 'таймаут' }), content: '' }),
            'summary/2': json({ meta: version(2, { status: 'failed' }), content: '' }),
        },
    });
    await p.open();
    assert.deepEqual(p.versionTabs(), ['v1 !', 'v2 !']);
    assert.equal(p.content(), 'Generation failed: Generation failed');
    versionButton(p, 0).click();
    await p.settle();
    assert.equal(p.content(), 'Generation failed: таймаут');
});

function versionButton(p, index) {
    return p.document.querySelectorAll('#version-tabs .version-tab-btn')[index];
}

test('version still generating - opened - waits, polls every two seconds and shows the result', async () => {
    let done = false;
    const p = aiPage({
        increments: { summary: [version(1, { status: 'generating' })] },
        contents: { 'summary/1': () => json(done ? { meta: version(1), content: 'Готово' } : { meta: version(1, { status: 'generating' }), content: '' }) },
    });
    await p.open();
    assert.deepEqual(p.versionTabs(), ['v1 ...']);
    assert.equal(p.content(), 'Generation in progress...');
    p.timers.advance(2000);
    await p.settle();
    assert.equal(p.content(), 'Generation in progress...');
    done = true;
    p.timers.advance(2000);
    await p.settle();
    assert.match(p.content(), /Готово$/);
    p.timers.advance(10000);
    assert.equal(p.requests.filter(r => r.url.endsWith('/summary/1')).length, 3);
});

test('version missing on the server or the request fails - opened - says which', async () => {
    const missing = aiPage({ contents: { 'summary/1': json({}, { status: 404 }) } });
    await missing.open();
    assert.equal(missing.content(), 'Version not found');
    const broken = aiPage({ contents: { 'summary/1': () => Promise.reject(new Error('offline')) } });
    await broken.open();
    assert.equal(broken.content(), 'Failed to load: offline');
});

test('two versions - the older tab clicked - that version is shown and its tab active', async () => {
    const p = aiPage({ increments: { summary: [version(1), version(2)] } });
    await p.open();
    assert.match(p.content(), /Ответ v2$/);
    versionButton(p, 0).click();
    await p.settle();
    assert.match(p.content(), /Ответ v1$/);
    assert.deepEqual(p.document.querySelectorAll('#version-tabs .version-tab-btn').map(btn => btn.classList.contains('active')), [true, false]);
});

test('saved results of two prompts - the second prompt tab clicked - its versions are shown', async () => {
    const p = aiPage({ increments: { summary: [version(1)], 'my-increment': [version(3)] } });
    await p.open();
    assert.equal(p.$('increment-participant-toolbar').style.display, 'none');
    p.$('ai-saved-view').querySelectorAll('.modal-tabs .tab-btn')[1].click();
    await p.settle();
    assert.deepEqual(p.promptTabs(), [['Итоги', false], ['Мой инкремент', true]]);
    assert.equal(p.$('increment-participant-toolbar').style.display, '');
    assert.match(p.content(), /Ответ v3$/);
});

test('meeting participants - my increment opened - the user first, then others once each, the system left out', async () => {
    const p = aiPage({ increments: { 'my-increment': [version(1)] } });
    await p.open();
    assert.deepEqual(p.participants(), [['', 'Вера'], ['anna', 'Анна'], ['boris', 'boris']]);
});

test('meeting without a local participant - my increment opened - the user is called Me', async () => {
    const p = aiPage({ increments: { 'my-increment': [version(1)] }, meta: { ...META, participants: undefined } });
    await p.open();
    assert.deepEqual(p.participants(), [['', 'Me']]);
});

test('saved version - Regenerate clicked - generated over the whole transcript and the new version shown', async () => {
    let increments = { summary: [version(1)] };
    const p = aiPage({
        routes: {
            'POST /api/meetings/m1/increments/summary/generate?force=true': async () => {
                increments = { summary: [version(1), version(2)] };
                return json({ status: 'completed' });
            },
            'GET /api/meetings/m1/increments': async () => json(increments),
        },
    });
    await p.open();
    p.footerButton('Regenerate').click();
    assert.equal(p.content(), 'Generating...');
    await p.settle();
    assert.deepEqual(p.posts(), ['/api/meetings/m1/increments/summary/generate?force=true']);
    assert.deepEqual(p.versionTabs(), ['v1', 'v2']);
    assert.match(p.content(), /Ответ v2$/);
});

for (const [answer, shown] of [
    [() => json({ detail: 'нет провайдера' }, { status: 503 }), 'Regeneration failed: нет провайдера'],
    [() => ({ ok: false, json: async () => { throw new Error('html'); } }), 'Regeneration failed: Generation failed'],
    [() => json({ status: 'skipped', message: 'Ничего нового' }), 'Ничего нового'],
    [() => json({ status: 'skipped' }), 'No new segments'],
]) {
    test(`saved version - Regenerate answered ${shown} - the answer is shown`, async () => {
        const p = aiPage({ routes: { 'POST /api/meetings/m1/increments/summary/generate*': answer } });
        await p.open();
        p.footerButton('Regenerate').click();
        await p.settle();
        assert.equal(p.content(), shown);
    });
}

test('modal never opened - regenerate or participant generation called - nothing is requested', async () => {
    const p = aiPage();
    await p.settle();
    p.evaluate('regeneratePrompt(); generateParticipantIncrement()');
    await p.settle();
    assert.deepEqual(p.posts(), []);
});

test('my increment without a saved version - Create clicked - generated for the user and shown', async () => {
    let increments = { 'my-increment': [version(1, { participant_id: 'anna' })] };
    const p = aiPage({
        routes: {
            'POST /api/meetings/m1/increments/my-increment/generate': async () => {
                increments = { 'my-increment': [...increments['my-increment'], version(2)] };
                return json({ status: 'completed' });
            },
            'GET /api/meetings/m1/increments': async () => json(increments),
        },
    });
    await p.open();
    assert.equal(p.content(), 'Сохранённых версий пока нет. Создать сохранённый результат');
    p.contentEl().querySelector('.btn').click();
    assert.equal(p.content(), 'Generating increment...');
    p.evaluate('generateParticipantIncrement()');
    await p.settle();
    assert.deepEqual(p.posts(), ['/api/meetings/m1/increments/my-increment/generate']);
    assert.match(p.content(), /Ответ v2$/);
});

test('another participant chosen - Create clicked - generated for that participant', async () => {
    const p = aiPage({
        increments: { 'my-increment': [version(1)] },
        routes: { 'POST /api/meetings/m1/increments/my-increment/generate*': async () => json({ status: 'completed' }) },
    });
    await p.open();
    p.choose('anna');
    await p.settle();
    p.contentEl().querySelector('.btn').click();
    await p.settle();
    assert.deepEqual(p.posts(), ['/api/meetings/m1/increments/my-increment/generate?participant_id=anna']);
});

for (const [answer, shown] of [
    [() => json({ detail: 'лимит' }, { status: 429 }), 'Generation failed: лимит'],
    [() => ({ ok: false, json: async () => { throw new Error('html'); } }), 'Generation failed: Generation failed'],
    [() => json({ status: 'skipped', message: 'Нет новых реплик' }), 'Generation failed: Нет новых реплик'],
    [() => json({ status: 'skipped' }), 'Generation failed: No new segments to process'],
]) {
    test(`my increment - Create answered with ${shown} - the reason and Create again`, async () => {
        const p = aiPage({
            increments: { 'my-increment': [version(1, { participant_id: 'anna' })] },
            routes: { 'POST /api/meetings/m1/increments/my-increment/generate': answer },
        });
        await p.open();
        p.contentEl().querySelector('.btn').click();
        await p.settle();
        assert.equal(p.content(), `${shown} Создать сохранённый результат`);
    });
}

test('summary prompt shown - participant generation called - nothing is requested', async () => {
    const p = aiPage();
    await p.open();
    await p.evaluate('generateParticipantIncrement()');
    assert.deepEqual(p.posts(), []);
});

// The new-analysis stream of the modal.
async function streaming() {
    const p = aiPage();
    await p.open();
    p.document.querySelector('[data-ai-mode="new"]').click();
    p.$('ai-run-button').click();
    return { p, stream: p.eventSources.at(-1) };
}

test('analysis started - a provider without a name streams and finishes - its id is the tab and the answer shown', async () => {
    const { p, stream } = await streaming();
    assert.equal(stream.url, '/api/meetings/m1/analyze-stream?question=increment');
    assert.equal(p.newContent(), 'Подключение...');
    stream.emit('message', { type: 'start', provider: 'codex' });
    assert.equal(p.newContent(), 'Model: codex Анализ...');
    stream.emit('message', { type: 'chunk', provider: 'claude', text: 'чужое' });
    stream.emit('message', { type: 'chunk', provider: 'codex', text: 'част' });
    assert.equal(p.text(p.$('ai-new-content').querySelector('.ai-answer')), '● част');
    stream.emit('message', { type: 'done', provider: 'codex', text: 'целое' });
    stream.emit('message', { type: 'complete' });
    assert.equal(p.text(p.$('ai-provider-tabs')), 'codex');
    assert.equal(p.newContent(), 'Model: codex целое');
    assert.equal(stream.closed, true);
    assert.equal(p.text(p.$('ai-run-button')), 'Запустить заново');
});

test('analysis running - a provider fails with or without a reason - shown under that provider', async () => {
    const { p, stream } = await streaming();
    stream.emit('message', { type: 'start', provider: 'codex', name: 'Codex' });
    stream.emit('message', { type: 'start', provider: 'claude', name: 'Claude' });
    stream.emit('message', { type: 'error', provider: 'codex', message: 'квота' });
    stream.emit('message', { type: 'error', provider: 'claude' });
    stream.emit('message', { type: 'progress' });
    assert.deepEqual(p.$('ai-new-content').querySelectorAll('.error').map(el => p.text(el)), ['квота', 'Анализ завершился с ошибкой']);
    assert.equal(stream.closed, false);
});

for (const [message, shown] of [['нет ключа', 'нет ключа'], [undefined, 'Не удалось запустить анализ']]) {
    test(`analysis started - rejected before any provider (${shown}) - the reason and Run again`, async () => {
        const { p, stream } = await streaming();
        stream.emit('message', { type: 'error', message });
        assert.equal(p.newContent(), shown);
        assert.equal(stream.closed, true);
        assert.equal(p.$('ai-run-button').disabled, false);
    });
}

test('analysis started - completes without any provider - says none is available', async () => {
    const { p, stream } = await streaming();
    stream.emit('message', { type: 'complete' });
    assert.equal(p.newContent(), 'Нет доступных провайдеров для анализа.');
});

test('analysis running - a frame that is not JSON - logged and the analysis goes on', async () => {
    const { p, stream } = await streaming();
    stream.emit('message', '{broken');
    assert.equal(p.errors.length, 1);
    stream.emit('message', { type: 'start', provider: 'codex', name: 'Codex' });
    assert.equal(p.newContent(), 'Model: Codex Анализ...');
});

test('analysis running - the connection drops - partial results stay with a note', async () => {
    const { p, stream } = await streaming();
    stream.emit('message', { type: 'start', provider: 'codex', name: 'Codex' });
    stream.emit('message', { type: 'chunk', provider: 'codex', text: 'частично' });
    stream.emit('error');
    assert.equal(p.newContent(), 'Соединение прервано. Полученные результаты показаны ниже; анализ можно запустить заново. Model: Codex частично');
});

test('history fails - opened - the reason and a retry that loads it', async () => {
    let fail = true;
    const p = aiPage({ routes: { 'GET /api/prompts': async () => (fail ? json({}, { status: 500 }) : json(PROMPTS)) } });
    await p.open();
    const body = p.$('ai-saved-view').querySelector('.modal-body');
    assert.equal(p.words(body), 'Не удалось загрузить сохранённые результаты: Failed to load prompts Повторить загрузку');
    fail = false;
    body.querySelector('.btn').click();
    await p.settle();
    assert.match(p.content(), /Ответ v1$/);
});

test('no saved results - opened - says so and switches to a new analysis', async () => {
    const p = aiPage({ increments: {} });
    await p.open();
    assert.equal(p.text(p.$('ai-saved-view').querySelector('.modal-body')), 'Сохранённых результатов пока нет.');
    assert.equal(p.$('ai-new-view').hidden, false);
    assert.equal(p.$('ai-saved-view').hidden, true);
});
