export function injectDebugStyles() {
  if (document.querySelector('#ktalk-debug-styles')) return;

  const styles = document.createElement('style');
  styles.id = 'ktalk-debug-styles';
  styles.textContent = `
      /* Debug panel */
      .ktalk-debug-toggle {
        position: fixed;
        right: 12px;
        bottom: 12px;
        z-index: 9999;
        padding: 6px 10px;
        background: rgba(0, 0, 0, 0.6);
        border: 1px solid rgba(255,255,255,0.2);
        border-radius: 6px;
        color: #fff;
        font-size: 11px;
        cursor: pointer;
      }
      .ktalk-debug-panel {
        position: fixed;
        right: 12px;
        bottom: 48px;
        width: 360px;
        max-height: 50vh;
        overflow: auto;
        z-index: 9999;
        background: rgba(17, 17, 17, 0.92);
        border: 1px solid rgba(255,255,255,0.2);
        border-radius: 8px;
        color: #e5e7eb;
        font-size: 11px;
        padding: 10px;
        display: none;
        font-family: "Lab Grotesque", Arial, sans-serif;
      }
      .ktalk-debug-panel.active {
        display: block;
      }
      .ktalk-debug-title {
        font-weight: 600;
        margin-bottom: 8px;
        color: #fff;
      }
      .ktalk-debug-row {
        display: flex;
        justify-content: space-between;
        gap: 8px;
        margin-bottom: 4px;
      }
      .ktalk-debug-key {
        color: rgba(255,255,255,0.6);
      }
      .ktalk-debug-value {
        color: #fff;
        text-align: right;
        max-width: 220px;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }
      .ktalk-debug-json {
        margin-top: 8px;
        padding: 6px;
        background: rgba(255,255,255,0.05);
        border-radius: 6px;
        white-space: pre-wrap;
        word-break: break-word;
      }
      .ktalk-debug-actions {
        display: flex;
        justify-content: flex-end;
        gap: 8px;
        margin-top: 8px;
      }
      .ktalk-debug-copy-btn {
        padding: 4px 8px;
        background: rgba(255,255,255,0.1);
        border: none;
        border-radius: 4px;
        color: rgba(255,255,255,0.8);
        font-size: 11px;
        cursor: pointer;
      }
      .ktalk-debug-copy-btn:hover {
        background: rgba(255,255,255,0.2);
      }
      .ktalk-debug-copy-btn.copied {
        background: #22c55e;
        color: #fff;
      }
    `;

  document.head.appendChild(styles);
}
