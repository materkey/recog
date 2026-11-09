const { test } = require('node:test');
const assert = require('node:assert/strict');
const { loadPage, json } = require('./web_page.cjs');

const MEETINGS = [
    { id: 'a', title: 'Утренний созвон', source: 'ktalk', created_at: '2026-11-09T09:00:00Z', participants: ['Анна', 'Борис', 'Вера', 'Глеб'], duration_seconds: 600, segment_count: 12, is_active: true },
    { id: 'b', title: 'Ретро', source: 'ktalk', created_at: '2026-11-08T15:00:00Z' },
    { id: 'c', title: null, created_at: '2026-11-07T15:00:00Z' },
];

function listPage({ meetings = MEETINGS, search = async () => json([]), local = {} } = {}) {
    const fetch = async (url) => {
        if (url === '/api/meetings') return json(meetings);
        if (url.startsWith('/api/search?')) return search(url);
        return json({}, { status: 404 });
    };
    const page = loadPage({ hash: '#/', ready: true, fetch, local: { filterSource: '', ...local } });
    const input = (id, value, type) => {
        const el = page.$(id);
        el.value = value;
        el.dispatchEvent({ type, target: el, bubbles: true });
    };
    return {
        ...page,
        input,
        cards: () => page.document.querySelectorAll('#meetings-list .meeting-card').map(card => page.text(card.querySelector('.meeting-card-title'))),
        settle: async () => { for (let i = 0; i < 5; i++) await page.flush(); },
    };
}

test('stored meetings - list opened - each card shows its badges, duration, segments and first participants', async () => {
    const p = listPage();
    await p.settle();
    assert.deepEqual(p.cards(), ['Утренний созвон', 'Ретро', 'Untitled']);
    const [live, plain, unknown] = p.document.querySelectorAll('#meetings-list .meeting-card');
    assert.equal(live.className, 'meeting-card meeting-card-active');
    assert.match(p.text(live), /^LIVE ktalk .* Утренний созвон ⏱ 10m 0s 💬 12 segments Анна, Борис, Вера \+1$/);
    assert.match(p.text(plain), /Ретро ⏱ - 💬 0 segments$/);
    assert.equal(unknown.querySelector('.meeting-source').className, 'meeting-source source-unknown');
    assert.equal(live.getAttribute('href'), '#/meeting/a');
});

test('title typed - content mode chosen - matching transcript lines are shown highlighted', async () => {
    const results = [{
        meeting_id: 'a', title: 'Утренний созвон', created_at: '2026-11-09T09:00:00Z',
        matches: [{ ts_display: '09:01', speaker: 'Анна', text: 'обсудим релиз' }],
    }];
    const p = listPage({ search: async () => json(results) });
    await p.settle();
    p.input('filter-title', 'релиз', 'input');
    p.input('search-mode', 'content', 'change');
    assert.equal(p.text(p.$('search-results')), 'Searching...');
    assert.equal(p.$('meetings-list').style.display, 'none');
    await p.settle();
    assert.equal(p.requests.at(-1).url, '/api/search?q=%D1%80%D0%B5%D0%BB%D0%B8%D0%B7');
    assert.match(p.text(p.$('search-results')), /Утренний созвон .* 09:01 Анна: обсудим релиз/);
    assert.equal(p.$('search-results').querySelector('mark').textContent, 'релиз');
    assert.equal(p.window.localStorage.getItem('searchMode'), 'content');
});

test('content mode - search finds nothing - says so', async () => {
    const p = listPage({ local: { searchMode: 'content' } });
    await p.settle();
    p.input('filter-title', 'нет', 'input');
    p.timers.advance(300);
    await p.settle();
    assert.equal(p.text(p.$('search-results')), 'No matches found');
});

test('content mode - search fails - the error is shown', async () => {
    const p = listPage({ local: { searchMode: 'content' }, search: async () => json({}, { status: 500 }) });
    await p.settle();
    p.input('filter-title', 'релиз', 'input');
    p.timers.advance(300);
    await p.settle();
    assert.equal(p.text(p.$('search-results')), 'Error: Search failed');
});

test('content mode - search answers a body that is not JSON - the parser error is shown as text', async () => {
    const unparsable = { ok: true, status: 200, json: async () => { throw new SyntaxError('Unexpected token \'<\', "<b>oops</b>" is not valid JSON'); } };
    const p = listPage({ local: { searchMode: 'content' }, search: async () => unparsable });
    await p.settle();
    p.input('filter-title', 'релиз', 'input');
    p.timers.advance(300);
    await p.settle();
    assert.equal(p.text(p.$('search-results')), 'Error: Unexpected token \'<\', "<b>oops</b>" is not valid JSON');
    assert.equal(p.$('search-results').querySelector('b'), null);
});

test('content mode - query shorter than two letters or title mode - the card list filtered by title is back', async () => {
    const p = listPage({ local: { searchMode: 'content' } });
    await p.settle();
    p.input('filter-title', 'р', 'input');
    p.timers.advance(300);
    assert.equal(p.$('search-results').style.display, 'none');
    assert.equal(p.$('meetings-list').style.display, '');
    assert.deepEqual(p.cards(), ['Утренний созвон', 'Ретро']);
    p.input('filter-title', 'ретро', 'input');
    p.input('search-mode', 'title', 'change');
    assert.deepEqual(p.cards(), ['Ретро']);
    assert.equal(p.requests.some(r => r.url.startsWith('/api/search')), false);
});

test('typing in content mode - a second key within 300 ms - one search for the last text', async () => {
    const p = listPage({ local: { searchMode: 'content' } });
    await p.settle();
    p.input('filter-title', 'ре', 'input');
    p.timers.advance(200);
    p.input('filter-title', 'рел', 'input');
    p.timers.advance(300);
    await p.settle();
    assert.deepEqual(p.requests.filter(r => r.url.startsWith('/api/search')).map(r => r.url), ['/api/search?q=%D1%80%D0%B5%D0%BB']);
    assert.equal(p.window.localStorage.getItem('filterTitle'), 'рел');
});

const day = date => new Date(`${date}T12:00:00`).toISOString();
const dated = [
    { id: 'x', title: 'Восьмое', source: 'ktalk', created_at: day('2026-11-08') },
    { id: 'y', title: 'Девятое', source: 'ktalk', created_at: day('2026-11-09') },
    { id: 'z', title: 'Без даты', source: 'ktalk', created_at: '' },
];

test('a date chosen - previous and next day clicked - only that day is listed and the date is kept', async () => {
    const p = listPage({ meetings: dated });
    await p.settle();
    p.input('filter-date', '2026-11-09', 'change');
    assert.deepEqual(p.cards(), ['Девятое']);
    p.$('date-prev').click();
    assert.equal(p.$('filter-date').value, '2026-11-08');
    assert.deepEqual(p.cards(), ['Восьмое']);
    p.$('date-next').click();
    p.$('date-next').click();
    assert.equal(p.$('filter-date').value, '2026-11-10');
    assert.equal(p.text(p.$('meetings-list')), 'No meetings found');
    assert.equal(p.window.localStorage.getItem('filterDate'), '2026-11-10');
});

test('a date chosen - date field double-clicked - every day is listed', async () => {
    const p = listPage({ meetings: dated, local: { filterDate: '2026-11-08' } });
    await p.settle();
    assert.equal(p.$('filter-date').value, '2026-11-08');
    assert.deepEqual(p.cards(), ['Восьмое']);
    p.$('filter-date').dispatchEvent({ type: 'dblclick', bubbles: true });
    assert.deepEqual(p.cards(), ['Восьмое', 'Девятое', 'Без даты']);
});

const localDate = date => [date.getFullYear(), String(date.getMonth() + 1).padStart(2, '0'), String(date.getDate()).padStart(2, '0')].join('-');

test('no date chosen - today or previous day clicked - counted from today', async () => {
    const p = listPage({ meetings: dated });
    await p.settle();
    p.$('date-today').click();
    assert.equal(p.$('filter-date').value, localDate(new Date()));
    p.$('filter-date').dispatchEvent({ type: 'dblclick', bubbles: true });
    p.$('date-prev').click();
    const yesterday = new Date();
    yesterday.setDate(yesterday.getDate() - 1);
    assert.equal(p.$('filter-date').value, localDate(yesterday));
});

test('list opened twice - date buttons clicked - each moves the date once', async () => {
    const p = listPage({ meetings: dated, local: { filterDate: '2026-11-09' } });
    await p.settle();
    p.setHash('#/live');
    p.setHash('#/');
    await p.settle();
    p.$('date-prev').click();
    assert.equal(p.$('filter-date').value, '2026-11-08');
});
