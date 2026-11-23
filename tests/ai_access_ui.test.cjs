// The AI functions level on the page (crates/recog-web/static/ai-access.js): the
// question a new installation gets, and the controls hidden while the level is off.

const { test } = require('node:test');
const assert = require('node:assert/strict');
const { loadPage, json } = require('./web_page.cjs');

const view = (access, revision) => ({ revision, access, models: {}, model_overrides: {}, tasks: [] });
const profile = (name) => ({
    profile: { name, aliases: [], configured: !!name, revision: name || 'unconfigured' },
    application_state: 'applied', suggested_name: '',
});

// The server: the level and its revision, the profile, and what each save sent.
function server({ access = 'unasked', name = 'Алиса', saveStatus = 200, increments = {} } = {}) {
    const state = { access, revision: 'r1', saves: [] };
    const fetch = async (url, options = {}) => {
        if (url === '/api/ai-settings/access') {
            const body = JSON.parse(options.body);
            state.saves.push(body);
            if (saveStatus !== 200) return json({ detail: 'nope' }, { status: saveStatus });
            state.access = body.access;
            state.revision = 'r2';
            return json(view(state.access, state.revision));
        }
        if (url.startsWith('/api/ai-settings')) return json(view(state.access, state.revision));
        if (url.startsWith('/api/user-profile')) return json(profile(name));
        if (url.endsWith('/increments')) return json({ increments, prompts: [] });
        return json({});
    };
    return { state, fetch };
}

async function page(options = {}, { session = {}, hash = '#/' } = {}) {
    const backend = server(options);
    const h = loadPage({ fetch: backend.fetch, ready: true, session, hash });
    for (let i = 0; i < 5; i++) await h.flush();
    return { h, backend };
}

const dialogOpen = h => h.$('ai-access-dialog').open === true;

test('capture listener on the document - click on a button with an inline onclick - the listener runs first and stopPropagation keeps the onclick from running', async () => {
    const h = loadPage({ fetch: async () => json({}) });
    const order = [];
    h.window.markInline = () => order.push('inline');
    const button = h.document.createElement('button');
    button.setAttribute('onclick', 'markInline()');
    h.document.body.append(button);
    h.document.addEventListener('click', event => {
        order.push('capture');
        event.stopPropagation();
    }, true);
    button.click();
    assert.deepEqual(order, ['capture']);
});

test('unasked - click on an AI button - question opens and the button handler does not run', async () => {
    const { h } = await page();
    h.$('ai-access-dialog').close();
    let analyses = 0;
    h.window.generateIncrementSSE = () => { analyses++; };
    h.$('ai-run-button').click();
    assert.equal(dialogOpen(h), true);
    assert.equal(analyses, 0);
    h.$('ai-access-dialog').close();
    h.$('meeting-chat-btn').click();
    assert.equal(dialogOpen(h), true);
    assert.equal(h.$('meeting-chat').hidden, true);
});

test('question answered manual - saved with a fresh revision - the original click runs once', async () => {
    const { h, backend } = await page();
    h.$('ai-access-dialog').close();
    let analyses = 0;
    h.window.generateIncrementSSE = () => { analyses++; };
    backend.state.revision = 'r-fresh';
    h.$('ai-run-button').click();
    h.$('ai-access-manual').click();
    for (let i = 0; i < 5; i++) await h.flush();
    assert.deepEqual(backend.state.saves, [{ access: 'manual', revision: 'r-fresh' }]);
    assert.equal(analyses, 1);
    assert.equal(dialogOpen(h), false);
    assert.equal(h.document.documentElement.dataset.aiAccess, 'manual');
});

test('question answered off - root marked off - the click does not run', async () => {
    const { h, backend } = await page();
    h.$('ai-access-dialog').close();
    let analyses = 0;
    h.window.generateIncrementSSE = () => { analyses++; };
    h.$('ai-run-button').click();
    h.$('ai-access-off').click();
    for (let i = 0; i < 5; i++) await h.flush();
    assert.equal(backend.state.saves[0].access, 'off');
    assert.equal(analyses, 0);
    assert.equal(h.document.documentElement.dataset.aiAccess, 'off');
});

test('level off - page loads - root carries data-ai-access off', async () => {
    const { h } = await page({ access: 'off' });
    assert.equal(h.document.documentElement.dataset.aiAccess, 'off');
    assert.equal(dialogOpen(h), false);
});

test('unasked and Later - page reloads in the same session - no automatic question', async () => {
    const first = await page();
    assert.equal(dialogOpen(first.h), true);
    first.h.$('ai-access-later').click();
    const session = { 'recog.ai-access.later': first.h.window.sessionStorage.getItem('recog.ai-access.later') };
    assert.equal(session['recog.ai-access.later'], '1');
    const second = await page({}, { session });
    assert.equal(dialogOpen(second.h), false);
});

test('unasked with the profile welcome open - welcome closes - question opens', async () => {
    const { h } = await page({ name: '' });
    assert.equal(h.$('profile-dialog').open, true);
    assert.equal(dialogOpen(h), false);
    h.$('profile-dialog').close();
    await h.flush();
    assert.equal(dialogOpen(h), true);
});

test('level off - AI Analysis opened on a meeting with saved summaries - they are shown and every control that generates carries ai-entry', async () => {
    const { h } = await page({ access: 'off' });
    for (const id of ['ai-run-button', 'meeting-chat-btn', 'copilot-btn']) {
        assert.ok(h.$(id).classList.contains('ai-entry'), id);
    }
    assert.ok(h.document.querySelector('#ai-modal [data-ai-mode="new"]').classList.contains('ai-entry'));
    const analysis = [...h.document.querySelectorAll('button')].find(button => h.text(button) === 'AI Analysis');
    assert.equal(analysis.classList.contains('ai-entry'), false);
    const css = require('node:fs').readFileSync(require('node:path').join(__dirname, '../crates/recog-web/static/style.css'), 'utf8');
    assert.match(css, /:root\[data-ai-access="off"\] \.ai-entry\s*\{\s*display: none !important;/);
    const app = require('node:fs').readFileSync(require('node:path').join(__dirname, '../crates/recog-web/static/app.js'), 'utf8');
    assert.match(app, /class="btn ai-entry" onclick="regeneratePrompt\(\)"/);
    assert.match(app, /class="btn ai-entry" onclick="generateParticipantIncrement\(\)"/);
    assert.match(app, /class="btn btn-generate-title ai-entry"/);
});

test('level off and nothing saved - AI Analysis opened - the saved view stays', async () => {
    const { h } = await page({ access: 'off' });
    const saved = h.$('ai-saved-view');
    h.evaluate('aiModalSession = { meetingId: "m1", mode: "saved" }');
    h.evaluate('showAiHistory(aiModalSession, document.getElementById("ai-saved-view"), {}, [], true)');
    assert.equal(h.evaluate('aiModalSession.mode'), 'saved');
    assert.match(h.text(saved), /Сохранённых результатов пока нет/);
});
