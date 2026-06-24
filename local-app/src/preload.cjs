const { contextBridge, ipcRenderer } = require('electron');

contextBridge.exposeInMainWorld('localPrint', {
  getState: () => ipcRenderer.invoke('service:get-state'),
  restart: () => ipcRenderer.invoke('service:restart'),
  openPrintSite: () => ipcRenderer.invoke('service:open-print-site'),
  openAdmin: () => ipcRenderer.invoke('service:open-admin'),
  onState: (callback) => {
    const listener = (_event, state) => callback(state);
    ipcRenderer.on('service:state', listener);
    return () => ipcRenderer.removeListener('service:state', listener);
  }
});
