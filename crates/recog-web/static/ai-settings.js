/* Revision-checked shared settings; drafts stay local until explicitly saved. */
(() => {
    const PROVIDERS = { claude: 'Claude', codex: 'Codex' };
    const el = (id) => document.getElementById('ai-settings-' + id);
    const globalInputs = new Map();
    const modelInputs = new Map();
    const providerInputs = new Map();
    let snapshot = null;
    let selectedId = null;
    let globalDirty = false;
    let taskDirty = false;
    let busy = false;
    let conflict = false;
    let generation = 0;
    let previousFocus = null;
    let pollTimer = null;
    let pollDelay = 5000;

    const dirty = () => globalDirty || taskDirty;
    const selectedTask = () => snapshot?.tasks.find(
        (task) => task.id === selectedId
    );

    function schedulePoll() {
        clearTimeout(pollTimer);
        pollTimer = null;
        if (!el('dialog').open || document.hidden || dirty() || conflict
                || (snapshot?.copilot_application || snapshot?.copilot)?.state !== 'pending') {
            pollDelay = 5000;
            return;
        }
        if (busy) return;
        pollTimer = setTimeout(() => {
            pollTimer = null;
            pollDelay = Math.min(pollDelay * 2, 30000);
            refresh();
        }, pollDelay);
    }

    function markDirty(kind) {
        if (kind === 'global') globalDirty = true;
        else taskDirty = true;
        schedulePoll();
    }

    function error(message = '') {
        el('error').textContent = message;
        el('error').hidden = !message;
    }

    function setConflict() {
        conflict = true;
        el('conflict').hidden = false;
        controls();
    }

    function controls() {
        const unavailable = busy || !snapshot;
        el('save-models').disabled = unavailable || conflict;
        el('save-task').disabled = unavailable || conflict || !selectedTask();
        el('reset').disabled = unavailable || conflict || !selectedTask();
        el('task').disabled = unavailable;
        el('prompt').disabled = unavailable;
        el('reasoning').disabled = unavailable;
        el('enabled').disabled = unavailable;
        el('claude-effort').disabled = unavailable;
        el('reload').disabled = busy;
        el('close').disabled = busy;
        for (const input of [...globalInputs.values(), ...modelInputs.values(), ...providerInputs.values()]) {
            input.disabled = unavailable;
        }
        schedulePoll();
    }

    function modelField(provider, prefix, onInput) {
        const label = document.createElement('label');
        const name = document.createElement('span');
        name.textContent = PROVIDERS[provider];
        const input = document.createElement('input');
        input.id = 'ai-settings-' + prefix + '-' + provider;
        input.type = 'text';
        input.maxLength = 160;
        input.autocomplete = 'off';
        input.spellcheck = false;
        input.addEventListener('input', onInput);
        label.append(name, input);
        return { label, input };
    }

    function timing(task) {
        if (task?.id === 'analysis.increment') return 'Общие настройки нового анализа и сохранённых инкрементов. Изменения применяются со следующего запроса. Для сохранённого результата используется Codex, если он выбран; иначе первый выбранный провайдер.';
        if (task?.id !== 'copilot') return 'Изменения применяются со следующего запроса. Уже запущенный анализ продолжит работу со своими настройками.';
        if (task.enabled === false) return 'Выключение останавливает сессию и блокирует новые запуски. Сохранённые промпт и модель будут использованы после включения Copilot.';
        const pending = (snapshot?.copilot_application || snapshot?.copilot)?.state === 'pending';
        return 'Инструкции Copilot применятся на следующем обрабатываемом событии; текущий ответ не прерывается. Модель изменится при запуске новой сессии.'
            + ' Глубина рассуждений также применяется при новой сессии.'
            + (pending ? ' Передача инструкций ожидает подключения Copilot.' : '');
    }

    function updateModelHints() {
        for (const [provider, input] of modelInputs) input.placeholder = snapshot.models[provider] || 'Общая модель';
    }

    function fillTask() {
        const task = selectedTask();
        el('task').value = selectedId || '';
        el('providers').replaceChildren();
        el('task-models').replaceChildren();
        modelInputs.clear();
        providerInputs.clear();
        el('prompt').value = task?.prompt || '';
        el('enabled-row').hidden = task?.id !== 'copilot';
        el('fallback-row').hidden = true;
        el('enabled').checked = task?.enabled !== false;
        el('provenance').textContent = task?.customized ? 'Сценарий настроен вручную.' : 'Сценарий использует настройки по умолчанию.';
        el('placeholders').textContent = placeholdersText(task);
        el('timing').textContent = timing(task);
        if (!task) return;
        for (const provider of task.allowed_providers) {
            if (PROVIDERS[provider]) addProviderChoice(task, provider);
        }
        el('fallback-row').hidden = task.multiple || providerInputs.size < 2;
        el('fallback').checked = task.providers.length > 1;
        updateModelHints();
        el('reasoning').value = task.reasoning_effort;
        el('claude-effort').value = task.claude_effort || '';
        updateReasoning();
    }

    function placeholdersText(task) {
        return task?.placeholders?.length
            ? 'Подстановки: ' + task.placeholders.map((name) => '{' + name + '}').join(', ')
                + '. Обязательные: ' + (task.required_placeholders || []).map((name) => '{' + name + '}').join(', ') + '.'
            : 'Текст инструкций передаётся буквально, без подстановок.';
    }

    // The provider's checkbox (or radio) and its scenario model field.
    function addProviderChoice(task, provider) {
        const label = document.createElement('label');
        const input = document.createElement('input');
        input.type = task.multiple ? 'checkbox' : 'radio';
        input.name = 'ai-settings-provider';
        input.value = provider;
        input.checked = task.multiple ? task.providers.includes(provider) : task.providers[0] === provider;
        input.addEventListener('change', () => {
            markDirty('task');
            updateReasoning();
        });
        const name = document.createElement('span');
        name.textContent = PROVIDERS[provider];
        label.append(input, name);
        el('providers').append(label);
        providerInputs.set(provider, input);
        const field = modelField(provider, 'model', () => markDirty('task'));
        field.input.value = task.model_overrides?.[provider] || '';
        modelInputs.set(provider, field.input);
        el('task-models').append(field.label);
    }

    // The providers in the order the scenario asks them: every checked one for an
    // analysis; otherwise the chosen one, then the other when it may stand in.
    function selectedProviders() {
        const chosen = [...providerInputs]
            .filter(([, input]) => input.checked)
            .map(([key]) => key);
        if (el('fallback-row').hidden || !el('fallback').checked || !chosen.length) return chosen;
        const others = [...providerInputs.keys()]
            .filter((key) => key !== chosen[0]);
        return [chosen[0], ...others];
    }

    function updateReasoning() {
        const providers = selectedProviders();
        el('reasoning-row').hidden = !providers.includes('codex');
        el('claude-effort-row').hidden = !providers.includes('claude');
    }

    function fillGlobals() {
        for (const [provider, input] of globalInputs) {
            input.value = snapshot.model_overrides?.[provider] || '';
            input.placeholder = snapshot.models[provider] || '';
        }
    }

    function render(data, { preserveGlobals = false, preserveTask = false } = {}) {
        snapshot = data;
        if (!data.tasks.some((task) => task.id === selectedId)) selectedId = data.tasks[0]?.id || null;
        el('task').replaceChildren();
        for (const task of data.tasks) {
            const option = document.createElement('option');
            option.value = task.id;
            option.textContent = task.label;
            el('task').append(option);
        }
        el('task').value = selectedId || '';
        if (!preserveGlobals) fillGlobals();
        if (!preserveTask) fillTask();
        else {
            updateModelHints();
            el('timing').textContent = timing(selectedTask());
        }
        controls();
    }

    async function request(path = '', options = {}) {
        const controller = new AbortController();
        const timer = setTimeout(() => controller.abort(), 10000);
        try {
            const response = await fetch('/api/ai-settings' + path, { cache: 'no-store', ...options, signal: controller.signal });
            if (!response.ok) {
                const failure = new Error('Не удалось прочитать или сохранить настройки. Попробуйте ещё раз.');
                failure.status = response.status;
                if (response.status === 422) {
                    const data = await response.json().catch(() => ({}));
                    failure.message = typeof data.detail === 'string' ? data.detail : 'Проверьте промпт, обязательные подстановки, провайдеры и имена моделей.';
                }
                throw failure;
            }
            const data = await response.json();
            if (typeof data.revision !== 'string' || !data.models || !Array.isArray(data.tasks)) {
                throw new Error('Сервис вернул некорректные настройки. Попробуйте обновить страницу.');
            }
            return data;
        } finally {
            clearTimeout(timer);
        }
    }

    function reportFailure(failure) {
        if (failure.status === 409) setConflict();
        else error(failure.name === 'AbortError'
            ? 'Сервер не ответил вовремя. Обновите настройки перед повторным сохранением.'
            : failure.message || 'Нет соединения с сервисом настроек.');
    }

    async function refresh() {
        if (busy || !el('dialog').open) return;
        const current = ++generation;
        busy = true;
        controls();
        try {
            const data = await request();
            if (current !== generation || !el('dialog').open) return;
            if (draftConflicts(data)) {
                setConflict();
                return;
            }
            render(data, { preserveGlobals: globalDirty, preserveTask: taskDirty });
            error();
        } catch (failure) {
            if (current === generation) reportFailure(failure);
        } finally {
            finishRefresh(current);
        }
    }

    // A draft was made on another revision, or its scenario no longer exists.
    function draftConflicts(data) {
        return dirty() && snapshot && (data.revision !== snapshot.revision
            || (taskDirty && !data.tasks.some((task) => task.id === selectedId)));
    }

    function finishRefresh(current) {
        if (current === generation) {
            busy = false;
            controls();
        }
    }

    async function open() {
        if (el('dialog').open) return;
        previousFocus = document.activeElement;
        document.getElementById('profile-settings').open = false;
        globalDirty = false;
        taskDirty = false;
        conflict = false;
        el('conflict').hidden = true;
        el('status').textContent = '';
        error();
        if (snapshot) render(snapshot);
        // A failed reopen must not make an old snapshot or abandoned draft writable.
        snapshot = null;
        el('dialog').showModal();
        await refresh();
        if (el('dialog').open) el('task').focus();
    }

    function close() {
        if (busy || (dirty() && !window.confirm('Закрыть настройки и потерять несохранённые изменения?'))) return;
        generation += 1;
        el('dialog').close();
        schedulePoll();
        const target = previousFocus === el('open')
            ? document.getElementById('profile-settings').querySelector('summary') : previousFocus;
        if (target?.isConnected) target.focus();
    }

    async function reload() {
        if (busy || (dirty() && !window.confirm('Загрузить актуальные настройки и заменить несохранённые изменения?'))) return;
        // Keep drafts and their revision until the new snapshot was actually received.
        busy = true;
        controls();
        error();
        try {
            const data = await request();
            globalDirty = false;
            taskDirty = false;
            conflict = false;
            el('conflict').hidden = true;
            el('status').textContent = 'Актуальные настройки загружены.';
            render(data);
        } catch (failure) {
            reportFailure(failure);
        } finally {
            busy = false;
            controls();
        }
    }

    async function save(kind) {
        if (busy || conflict || !snapshot) return;
        const target = saveTarget(kind);
        if (!target) return;
        const { path, method, body } = target;
        busy = true;
        controls();
        error();
        try {
            const data = await request(path, { method, ...(body ? {
                headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body),
            } : {}) });
            if (kind === 'models') globalDirty = false;
            else taskDirty = false;
            render(data, { preserveGlobals: globalDirty, preserveTask: taskDirty });
            el('status').textContent = savedStatus(kind);
        } catch (failure) {
            reportFailure(failure);
        } finally {
            busy = false;
            controls();
        }
    }

    // The request a Save or Reset makes; null when it must not be made.
    function saveTarget(kind) {
        if (kind === 'models') {
            const body = { models: Object.fromEntries([...globalInputs].map(([key, input]) => [key, input.value.trim()])), revision: snapshot.revision };
            return { path: '/models', method: 'PUT', body };
        }
        if (!selectedTask()) return null;
        const path = '/tasks/' + encodeURIComponent(selectedId);
        if (kind === 'reset') {
            if (!window.confirm('Сбросить промпт, провайдеры и модели выбранного сценария к настройкам по умолчанию? Несохранённые изменения сценария будут потеряны.')) return null;
            return { path: path + '?revision=' + encodeURIComponent(snapshot.revision), method: 'DELETE', body: undefined };
        }
        const body = taskSaveBody();
        return body ? { path, method: 'PUT', body } : null;
    }

    function taskSaveBody() {
        const providers = selectedProviders();
        if (!providers.length) { error('Выберите хотя бы одного провайдера.'); return null; }
        const body = {
            prompt: el('prompt').value, providers,
            models: Object.fromEntries([...modelInputs].map(([key, input]) => [key, input.value.trim()])),
            revision: snapshot.revision,
        };
        if (selectedId === 'copilot') body.enabled = el('enabled').checked;
        if (selectedTask().allowed_providers.includes('codex')) {
            body.reasoning_effort = el('reasoning').value;
        }
        if (selectedTask().allowed_providers.includes('claude')) {
            body.claude_effort = el('claude-effort').value;
        }
        return body;
    }

    function savedStatus(kind) {
        return kind === 'models' ? 'Общие модели сохранены. Новые запросы используют эти модели, Copilot — новая сессия.'
            : (kind === 'reset' ? 'Настройки сценария сброшены.' : 'Настройки сценария сохранены.');
    }

    document.addEventListener('DOMContentLoaded', () => {
        for (const provider of Object.keys(PROVIDERS)) {
            const field = modelField(provider, 'global', () => markDirty('global'));
            globalInputs.set(provider, field.input);
            el('global-models').append(field.label);
        }
        el('open').addEventListener('click', open);
        el('close').addEventListener('click', close);
        el('dialog').addEventListener('cancel', (event) => { event.preventDefault(); close(); });
        el('prompt').addEventListener('input', () => markDirty('task'));
        el('reasoning').addEventListener('change', () => markDirty('task'));
        el('enabled').addEventListener('change', () => markDirty('task'));
        el('claude-effort').addEventListener('change', () => markDirty('task'));
        el('fallback').addEventListener('change', () => {
            markDirty('task');
            updateReasoning();
        });
        el('task').addEventListener('change', () => {
            if (taskDirty && !window.confirm('Переключить сценарий и потерять его несохранённые изменения?')) {
                el('task').value = selectedId;
                return;
            }
            selectedId = el('task').value;
            taskDirty = false;
            error();
            el('status').textContent = '';
            fillTask();
            controls();
        });
        el('save-models').addEventListener('click', () => save('models'));
        el('save-task').addEventListener('click', () => save('task'));
        el('reset').addEventListener('click', () => save('reset'));
        el('reload').addEventListener('click', reload);
        window.addEventListener('focus', refresh);
        document.addEventListener('visibilitychange', () => {
            schedulePoll();
            if (!document.hidden) refresh();
        });
        window.addEventListener('beforeunload', (event) => {
            if (el('dialog').open && dirty()) { event.preventDefault(); event.returnValue = ''; }
        });
        controls();
    });
})();
