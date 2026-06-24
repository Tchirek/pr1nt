import { getCloudflareContext } from "@opennextjs/cloudflare";
import { fetchRequestHandler } from "@trpc/server/adapters/fetch";

import { appRouter, createTrpcContext, type AppBindings } from "../../../../server/trpc/router";

function handler(request: Request): Promise<Response> {
  const { env } = getCloudflareContext();

  return fetchRequestHandler({
    endpoint: "/api/trpc",
    req: request,
    router: appRouter,
    createContext: () => createTrpcContext(request, env as AppBindings),
  });
}

export { handler as GET, handler as POST };
