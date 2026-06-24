609 Local Print Server Deployment

Preferred deployment

Double-click the portable desktop app:

   Room 101 Print Service.exe

The desktop app starts local-server.exe in the background and keeps the existing
local admin interface. You do not need to open a browser or start a Tunnel for
document upload.

Reliable transfer architecture

- Browsers upload original documents directly to the photohost R2 bucket's
  isolated print-staging/ prefix with a short-lived
  presigned PUT URL.
- The website Worker stores document and print-job metadata in the dedicated
  609-print D1 database.
- This local server connects outward to https://print.example.com, catches up
  pending work, downloads documents from R2, converts them, counts real pages,
  and confirms the result.
- Cloudflare Tunnel is not in the document byte path. The old HTTP/WebSocket
  upload endpoints remain available only for cached legacy pages.
- KV remains available for configuration and legacy queue compatibility. New
  document and print-job state is not maintained in KV.
- R2 source objects are deleted after a successful print. A seven-day R2
  lifecycle rule is the final cleanup fallback.

Required local-server .env values

   PRINT_WORKER_BASE_URL=https://print.example.com
   PRINT_SYNC_DEVICE_ID=609-main
   PRINT_SYNC_SECRET=replace-with-the-worker-print-sync-secret

PRINT_SYNC_DEVICE_ID is a stable location prefix. localserver appends the
Windows COMPUTERNAME automatically, so copied deployments cannot accidentally
share one claim identity. Keep computer names unique. Set PRINT_SYNC_INSTANCE_ID
only when a stable explicit suffix should override the computer name.
The Worker accepts the new suffixed identity for documents and jobs prepared by
the old unsuffixed prefix, so upgrading does not strand existing ready work.
PRINT_SYNC_SECRET must exactly match the Worker secret with the same name.

Legacy folder deployment

1. Extract the zip anywhere, for example:

   C:\safe\609\

2. Keep this folder layout:

   C:\safe\609\local-server\
   C:\safe\609\local-admin\

3. Edit local-server\.env if you need to change printer names, the document
   converter, the Worker URL, the stable device ID, or secrets.

4. Legacy only: double-click local-server\start-local-server.bat

5. Open the local admin page:

   http://127.0.0.1:8789/admin

Operational behavior

- Startup immediately performs a catch-up request.
- SSE is only a wake-up hint; a periodic poll is always active as a fallback.
- Downloaded files are streamed to .part files while SHA-256 is calculated,
  then atomically renamed before conversion.
- Claim leases prevent multiple print computers from converting the same
  document or printing the same queued job.
- Sync-managed local job journals prevent a completed job from being printed
  again if a Worker status update temporarily fails.
- A restart during printing marks the job failed instead of automatically
  reprinting it, because avoiding duplicate physical output is safer.
- Prepared PDFs are kept locally so queued jobs can resume after a restart.

Optional KV configuration sync

These values are still supported for the local admin configuration panel:

   CLOUDFLARE_ACCOUNT_ID
   CLOUDFLARE_KV_NAMESPACE_ID
   CLOUDFLARE_API_TOKEN

They are not required for the new document preparation or print-job workflow.
