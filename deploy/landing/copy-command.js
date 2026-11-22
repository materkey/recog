for (const block of document.querySelectorAll('.install-command')) {
  const button = block.querySelector('button');
  const command = block.querySelector('code');
  button.hidden = false;
  button.addEventListener('click', async () => {
    try {
      await navigator.clipboard.writeText(command.textContent);
      button.textContent = 'Скопировано';
    } catch {
      window.getSelection().selectAllChildren(command);
      button.textContent = 'Выделено для копирования';
    }
  });
}
