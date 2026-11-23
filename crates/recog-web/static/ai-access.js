/* The AI functions level on the page: what starts an AI path is hidden while it is
   off (style.css, .ai-entry), and a new installation is asked before the first one. */
(() => {
    const LATER_KEY = 'recog.ai-access.later';
    const LEVELS = ['unasked', 'off', 'manual', 'automatic'];
    const el = (id) => document.getElementById('ai-access-' + id);
    // Unknown until the settings answer; the server refuses on its own meanwhile.
    let level = null;
    let pending = null;

    function set(next) {
        level = LEVELS.includes(next) ? next : null;
        if (level) document.documentElement.dataset.aiAccess = level;
        else delete document.documentElement.dataset.aiAccess;
    }

    function laterChosen() {
        try { return sessionStorage.getItem(LATER_KEY) === '1'; } catch (_) { return false; }
    }

    function rememberLater() {
        try { sessionStorage.setItem(LATER_KEY, '1'); } catch (_) { /* Storage may be disabled. */ }
    }

    function showError(message = '') {
        el('error').textContent = message;
        el('error').hidden = !message;
    }

    function ask(target = null) {
        pending = target;
        showError();
        if (!el('dialog').open) el('dialog').showModal();
    }

    async function saveChoice(choice) {
        const current = await (await fetch('/api/ai-settings', { cache: 'no-store' })).json();
        const response = await fetch('/api/ai-settings/access', {
            method: 'PUT',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ access: choice, revision: current.revision }),
        });
        if (!response.ok) throw new Error('save failed');
        return (await response.json()).access;
    }

    // The answer, saved; the click it held back runs when it allows AI.
    async function choose(choice) {
        try {
            set(await saveChoice(choice));
        } catch (_) {
            showError('Не удалось сохранить выбор. Попробуйте ещё раз.');
            return;
        }
        const target = pending;
        pending = null;
        el('dialog').close();
        if (target && level !== 'off') target.click();
    }

    function later() {
        pending = null;
        rememberLater();
        el('dialog').close();
    }

    // A click on an AI control while unasked asks first.
    function intercept(event) {
        if (level !== 'unasked') return;
        const target = event.target?.closest?.('.ai-entry');
        if (!target) return;
        event.preventDefault();
        event.stopPropagation();
        ask(target);
    }

    // The question of a new installation, after the profile's welcome when it opens.
    async function offer() {
        await window.recogWelcomeDecided;
        if (level !== 'unasked' || laterChosen()) return;
        const welcome = document.getElementById('profile-dialog');
        if (welcome?.open) welcome.addEventListener('close', offer, { once: true });
        else ask();
    }

    async function load() {
        try {
            const response = await fetch('/api/ai-settings', { cache: 'no-store' });
            if (!response.ok) return;
            set((await response.json()).access);
        } catch (_) {
            return;
        }
        offer();
    }

    window.recogAiAccess = { level: () => level, set };
    document.addEventListener('click', intercept, true);
    document.addEventListener('DOMContentLoaded', () => {
        for (const choice of ['off', 'manual', 'automatic']) {
            el(choice).addEventListener('click', () => choose(choice));
        }
        el('later').addEventListener('click', later);
        el('dialog').addEventListener('cancel', (event) => {
            event.preventDefault();
            later();
        });
        load();
    });
})();
