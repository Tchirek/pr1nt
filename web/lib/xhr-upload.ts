export interface UploadProgressSnapshot {
  loaded: number;
  total: number | null;
  percent: number | null;
}

export interface UploadRequestOptions {
  url: string;
  formData?: FormData;
  body?: XMLHttpRequestBodyInit;
  headers?: Record<string, string>;
  responseType?: XMLHttpRequestResponseType;
  timeoutMs?: number;
  uploadStallTimeoutMs?: number;
  onUploadProgress?: (progress: UploadProgressSnapshot) => void;
  onUploadComplete?: () => void;
}

export interface UploadResponse {
  status: number;
  ok: boolean;
  response: XMLHttpRequest["response"];
  responseText: string;
  getHeader(name: string): string | null;
}

export function xhrUpload({
  url,
  formData,
  body,
  headers,
  responseType = "blob",
  timeoutMs,
  uploadStallTimeoutMs,
  onUploadProgress,
  onUploadComplete,
}: UploadRequestOptions): Promise<UploadResponse> {
  return new Promise((resolve, reject) => {
    const request = new XMLHttpRequest();
    let settled = false;
    let stallTimer: number | null = null;

    const clearStallTimer = () => {
      if (stallTimer !== null) {
        window.clearTimeout(stallTimer);
        stallTimer = null;
      }
    };

    const fail = (error: Error) => {
      if (settled) {
        return;
      }
      settled = true;
      clearStallTimer();
      reject(error);
    };

    const armStallTimer = () => {
      if (!uploadStallTimeoutMs || uploadStallTimeoutMs <= 0) {
        return;
      }
      clearStallTimer();
      stallTimer = window.setTimeout(() => {
        request.abort();
        fail(new Error("文件传输停滞，请检查网络后重试。"));
      }, uploadStallTimeoutMs);
    };

    request.open("POST", url, true);
    request.responseType = responseType;
    if (timeoutMs && timeoutMs > 0) {
      request.timeout = timeoutMs;
    }

    Object.entries(headers ?? {}).forEach(([key, value]) => {
      request.setRequestHeader(key, value);
    });

    request.upload.onprogress = (event) => {
      armStallTimer();
      if (!onUploadProgress) {
        return;
      }
      const total = event.lengthComputable ? event.total : null;
      onUploadProgress({
        loaded: event.loaded,
        total,
        percent: total && total > 0 ? Math.min(100, Math.round((event.loaded / total) * 100)) : null,
      });
    };

    request.upload.onload = () => {
      clearStallTimer();
      onUploadComplete?.();
    };

    request.onerror = () => {
      fail(new Error("网络请求失败，请检查连接状态。"));
    };

    request.ontimeout = () => {
      fail(new Error("请求超时，请稍后再试。"));
    };

    request.onabort = () => {
      fail(new Error("请求已取消。"));
    };

    request.onload = () => {
      if (settled) {
        return;
      }
      settled = true;
      clearStallTimer();
      void readResponseText(request, responseType)
        .then((responseText) => {
          resolve({
            status: request.status,
            ok: request.status >= 200 && request.status < 300,
            response: request.response,
            responseText,
            getHeader: (name: string) => request.getResponseHeader(name),
          });
        })
        .catch((error: unknown) => {
          reject(error instanceof Error ? error : new Error("读取服务器响应失败。"));
        });
    };

    armStallTimer();
    request.send(body ?? formData ?? null);
  });
}

async function readResponseText(
  request: XMLHttpRequest,
  responseType: XMLHttpRequestResponseType,
): Promise<string> {
  if (responseType === "" || responseType === "text") {
    return request.responseText;
  }

  if (request.status >= 200 && request.status < 300) {
    return "";
  }

  if (request.response instanceof Blob) {
    return request.response.text();
  }

  return typeof request.response === "string" ? request.response : "";
}
