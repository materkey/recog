/* Read-only KTalk Desktop integration status in the settings menu. */
(() => {
    const settings = document.getElementById('profile-settings');
    const status = document.getElementById('desktop-status');
    const labels = {
        disabled: 'выключена',
        waiting: 'ожидание приложения',
        attaching: 'подключение расширения',
        attached: 'расширение подключено',
        incompatible: 'несовместимая версия',
        error: 'ошибка подключения',
        inspector_unclosed: 'закрытие инспектора не подтверждено',
        server_offline: 'сервер недоступен',
    };
    const reasons = {
        inspector_disabled_or_unverified: 'поддержка инспектора не подтверждена',
        inspector_port_busy: 'порт инспектора занят',
        application_not_ready_or_debugging: 'приложение запускается или уже отлаживается',
        invalid_installation: 'проверьте установку приложения',
        invalid_configuration: 'проверьте настройку интеграции',
        attachment_timeout: 'время подключения истекло',
        page_initialization_failed: 'не удалось запустить страницу',
        cleanup_failed: 'после звонка перезапустите Толк',
        multiple_processes: 'найдено несколько процессов Толк',
        web_server_not_running: 'сервер Recog недоступен',
        observer_failed: 'ошибка наблюдателя, подробности в логе web-сервера',
    };
    let timer = null;
    let request = null;
    let generation = 0;

    function stop() {
        generation++;
        clearTimeout(timer);
        timer = null;
        request?.abort();
        request = null;
    }

    async function refresh() {
        if (!settings.open || document.hidden) return;
        const current = generation;
        const controller = new AbortController();
        request = controller;
        try {
            const response = await fetch('/api/desktop/status', {
                cache: 'no-store', signal: controller.signal,
            });
            if (!response.ok) throw new Error('status request failed');
            const data = await response.json();
            if (current !== generation) return;
            const state = data.state === 'inspector_unclosed' ? data.state
                : data.enabled ? data.state : 'disabled';
            let message = `KTalk Desktop: ${labels[state] || labels.error}`;
            if (data.app_version) message += ` · ${data.app_version}`;
            if (reasons[data.reason]) message += ` · ${reasons[data.reason]}`;
            status.textContent = message;
        } catch (error) {
            if (current === generation && error.name !== 'AbortError') status.textContent = 'KTalk Desktop: сервер недоступен';
        } finally {
            if (current === generation) {
                request = null;
                if (settings.open && !document.hidden) timer = setTimeout(refresh, 5000);
            }
        }
    }

    settings.addEventListener('toggle', () => {
        stop();
        if (settings.open && !document.hidden) refresh();
    });
    document.addEventListener('visibilitychange', () => {
        stop();
        if (!document.hidden && settings.open) refresh();
    });
})();
