(() => {
    const panel = document.getElementById('installation-readiness');
    const list = document.getElementById('installation-checks');
    const permissions = document.getElementById('macos-permissions');
    const status = document.getElementById('macos-permissions-status');
    panel.addEventListener('toggle', async () => {
        permissions.hidden = true;
        status.textContent = '';
        if (!panel.open) return;
        list.textContent = 'Проверяем…';
        try {
            const response = await fetch('/api/readiness', { cache: 'no-store' });
            if (!response.ok) throw new Error();
            const rows = await response.json();
            list.replaceChildren(...rows.map(row => {
                const item = document.createElement('li');
                const status = row.ready ? 'готово' : row.required ? 'требует настройки' : 'необязательно';
                item.textContent = row.name + ': ' + status + (row.hint ? ' · ' + row.hint : '');
                return item;
            }));
            permissions.hidden = window.self !== window.top || response.headers.get('recog-permissions-origin') !== location.origin;
        } catch {
            list.textContent = 'Проверка недоступна. Запустите recog doctor в терминале.';
        }
    });

    permissions.addEventListener('click', async event => {
        if (!event.isTrusted || !panel.open || permissions.hidden || permissions.disabled) return;
        permissions.disabled = true;
        status.textContent = '';
        try {
            const response = await fetch('/api/readiness/macos-permissions', {
                method: 'POST', mode: 'same-origin', redirect: 'error',
            });
            if (response.status === 409) {
                status.textContent = 'Окно разрешений уже открыто.';
            } else if (!response.ok) {
                throw new Error();
            }
        } catch {
            status.textContent = 'Не удалось открыть окно разрешений.';
        } finally {
            permissions.disabled = false;
        }
    });
})();
