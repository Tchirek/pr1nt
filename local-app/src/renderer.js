const statusCard = document.getElementById('status');
const statusMessage = document.getElementById('status-message');
const statusLabel = document.querySelector('.status-label');
const frameWrap = document.querySelector('.admin-frame-wrap');
const frame = document.getElementById('admin-frame');
const restartButton = document.getElementById('restart');
const siteButton = document.getElementById('site');

const labels = {
  starting: '正在启动',
  checking: '正在检查',
  warning: '需要注意',
  ready: '运行中',
  failed: '未运行'
};

function applyState(state) {
  const phase = state.phase || 'starting';
  statusCard.dataset.phase = phase;
  statusLabel.textContent = labels[phase] || labels.starting;
  statusMessage.textContent = state.message || '';

  if (phase === 'ready' && state.adminUrl) {
    if (frame.src !== state.adminUrl) frame.src = state.adminUrl;
    frameWrap.classList.add('ready');
  } else {
    frameWrap.classList.remove('ready');
  }
}

window.localPrint.onState(applyState);
window.localPrint.getState().then(applyState);

restartButton.addEventListener('click', () => {
  void window.localPrint.restart().then(applyState);
});

siteButton.addEventListener('click', () => {
  void window.localPrint.openPrintSite();
});
