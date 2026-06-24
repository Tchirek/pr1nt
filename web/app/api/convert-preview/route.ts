import { NextResponse } from "next/server";

export async function POST() {
  return NextResponse.json(
    {
      error:
        "文件预览转换只允许通过 Cloudflare Tunnel 直传到本地打印服务。请检查 LOCAL_SERVER_BASE_URL 配置。",
    },
    { status: 410 },
  );
}
