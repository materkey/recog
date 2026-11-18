(() => {
    const panel = document.getElementById('installation-readiness');
    const list = document.getElementById('installation-checks');
    panel.addEventListener('toggle', async () => {
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
        } catch {
            list.textContent = 'Проверка недоступна. Запустите recog doctor в терминале.';
        }
    });
})();
