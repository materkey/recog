/* Shared local identity; this dialog never records or enrolls a voice. */
(() => {
    const LATER_KEY = 'recog.user-profile.later';
    const el = (id) => document.getElementById('profile-' + id);
    let snapshot = null;
    let firstRead = true;
    let dismissed = false;
    let welcome = false;
    let dirty = false;
    let busy = false;
    let previousFocus = null;
    let generation = 0;
    let readPromise = null;
    let pollTimer = null;
    let pollDelay = 5000;
    let stopped = false;

    function suppressWelcome() {
        dismissed = true;
        try { sessionStorage.setItem(LATER_KEY, '1'); } catch (_) { /* Storage may be disabled. */ }
    }

    function showError(message = '') {
        el('error').textContent = message;
        el('error').hidden = !message;
    }

    function applicationMessage(data, saved = false) {
        if (!data.profile.configured && data.profile.revision === 'unconfigured') return '';
        if (data.application_state === 'pending') {
            return (data.profile.configured ? 'Профиль сохранён.' : 'Профиль очищен.')
                + ' Применение к Live и Copilot ожидает подключения; повторно сохранять не нужно.';
        }
        if (!data.profile.configured) {
            return saved ? 'Профиль очищен. Live и Copilot используют имя по умолчанию.' : '';
        }
        return saved ? 'Профиль сохранён и применён к Live и Copilot.' : '';
    }

    function render(data, saved = false) {
        const wasPending = snapshot?.application_state === 'pending';
        snapshot = data;
        el('setup').hidden = data.profile.configured;
        el('clear').hidden = !data.profile.configured;
        const message = applicationMessage(data, saved || wasPending);
        el('notice').textContent = message;
        el('notice').hidden = !message;
        el('dialog-status').textContent = applicationMessage(data);
        if (el('dialog').open && !dirty) fillForm();
        updateControls();
        schedulePoll();
    }

    function fillForm() {
        const profile = snapshot?.profile;
        const suggestion = (snapshot?.suggested_name || '').trim();
        el('name').value = profile?.configured ? profile.name : (/^me$/i.test(suggestion) ? '' : suggestion);
        el('aliases').value = (profile?.aliases || []).join('\n');
    }

    function updateControls() {
        el('save').disabled = busy || !snapshot;
        el('clear').disabled = busy || !snapshot;
        el('name').disabled = busy;
        el('aliases').disabled = busy;
        el('save').textContent = busy ? 'Сохранение…' : 'Сохранить';
    }

    function schedulePoll() {
        clearTimeout(pollTimer);
        pollTimer = null;
        if (stopped || document.hidden || snapshot?.application_state !== 'pending'
                || snapshot.profile.revision === 'unconfigured') {
            pollDelay = 5000;
            return;
        }
        pollTimer = setTimeout(() => {
            pollTimer = null;
            pollDelay = Math.min(pollDelay * 2, 30000);
            refresh();
        }, pollDelay);
    }

    async function request(options) {
        const controller = new AbortController();
        const timeout = setTimeout(() => controller.abort(), 10000);
        try {
            const response = await fetch('/api/user-profile', {
                cache: 'no-store', ...options, signal: controller.signal,
            });
            if (!response.ok) {
                if (response.status === 422) throw new Error('Проверьте имя и варианты обращения: до 80 символов каждый, не больше 16 вариантов. Имя «Me» использовать нельзя.');
                throw new Error('Сервис профиля недоступен. Попробуйте ещё раз.');
            }
            const data = await response.json();
            if (!data.profile || typeof data.profile.configured !== 'boolean'
                    || !['applied', 'pending'].includes(data.application_state)) {
                throw new Error('Не удалось прочитать профиль. Попробуйте ещё раз.');
            }
            return data;
        } finally {
            clearTimeout(timeout);
        }
    }

    function refresh() {
        if (stopped || busy) return Promise.resolve();
        if (readPromise) return readPromise;
        const readGeneration = generation;
        readPromise = (async () => {
            try {
                const data = await request();
                if (readGeneration !== generation || stopped) return;
                render(data);
                if (firstRead) {
                    firstRead = false;
                    if (!data.profile.configured && !dismissed && !el('dialog').open) open(true);
                }
            } catch (_) {
                if (readGeneration !== generation || stopped) return;
                if (el('dialog').open) showError('Не удалось обновить профиль. Проверьте подключение и откройте настройки снова.');
            } finally {
                readPromise = null;
                schedulePoll();
            }
        })();
        return readPromise;
    }

    function open(isWelcome = false) {
        if (el('dialog').open) return;
        welcome = isWelcome;
        dirty = false;
        previousFocus = document.activeElement;
        el('settings').open = false;
        el('title').textContent = welcome ? 'Как к вам обращаться?' : 'Мой профиль';
        el('later').textContent = welcome ? 'Позже' : 'Отмена';
        showError();
        fillForm();
        updateControls();
        el('dialog').showModal();
        el('name').focus();
        if (!isWelcome) refresh();
    }

    function close() {
        if (welcome) suppressWelcome();
        el('dialog').close();
        // A Settings item becomes hidden when its details menu closes.
        const target = previousFocus === el('open') ? el('settings').querySelector('summary') : previousFocus;
        if (target?.isConnected) target.focus();
    }

    async function mutate(method) {
        if (busy) return;
        if (method === 'DELETE' && !window.confirm('Очистить имя и все варианты обращения? Live и Copilot вернутся к имени по умолчанию. Прошлые записи сохранятся.')) return;
        const body = method === 'PUT' ? {
            name: el('name').value.trim(),
            aliases: el('aliases').value.split(/\r?\n/).map((value) => value.trim()).filter(Boolean),
        } : null;
        if (body && (!body.name || /^me$/i.test(body.name))) {
            showError('Укажите ваше имя вместо «Me».');
            el('name').focus();
            return;
        }
        busy = true;
        generation += 1;
        clearTimeout(pollTimer);
        showError();
        updateControls();
        try {
            const data = await request({ method, ...(body ? {
                headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body),
            } : {}) });
            dirty = false;
            suppressWelcome();
            render(data, true);
            if (el('dialog').open) close();
        } catch (error) {
            const message = error.name === 'AbortError'
                ? 'Сервер не ответил вовремя. Проверьте профиль перед повторным сохранением.'
                : (error instanceof TypeError ? 'Не удалось сохранить профиль. Проверьте подключение и попробуйте ещё раз.'
                    : (error.message || 'Не удалось сохранить профиль. Попробуйте ещё раз.'));
            showError(message);
            if (!el('dialog').open) {
                el('notice').textContent = message;
                el('notice').hidden = false;
            }
        } finally {
            busy = false;
            updateControls();
            schedulePoll();
        }
    }

    document.addEventListener('DOMContentLoaded', () => {
        try { dismissed = sessionStorage.getItem(LATER_KEY) === '1'; } catch (_) { /* Keep the in-memory dismissal. */ }
        el('open').addEventListener('click', () => open());
        el('setup').addEventListener('click', () => open());
        el('later').addEventListener('click', close);
        el('close').addEventListener('click', close);
        el('dialog').addEventListener('cancel', (event) => { event.preventDefault(); close(); });
        el('form').addEventListener('submit', (event) => { event.preventDefault(); return mutate('PUT'); });
        el('clear').addEventListener('click', () => mutate('DELETE'));
        for (const id of ['name', 'aliases']) el(id).addEventListener('input', () => { dirty = true; });
        window.addEventListener('focus', () => refresh());
        document.addEventListener('visibilitychange', () => {
            if (document.hidden) { clearTimeout(pollTimer); pollTimer = null; }
            else refresh();
        });
        window.addEventListener('pagehide', () => { stopped = true; clearTimeout(pollTimer); });
        window.addEventListener('pageshow', () => { stopped = false; refresh(); });
        refresh();
    });

    window.userProfile = { refresh };
})();
