const copyButton = document.getElementById('copy-command');
const command = document.getElementById('linux-command');

copyButton.hidden = false;
copyButton.addEventListener('click', async () => {
  try {
    await navigator.clipboard.writeText(command.textContent);
    copyButton.textContent = 'Скопировано';
  } catch {
    const selection = window.getSelection();
    selection.selectAllChildren(command);
    copyButton.textContent = 'Выделено для копирования';
  }
});
