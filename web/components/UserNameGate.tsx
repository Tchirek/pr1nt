"use client";

import { LoaderCircle, PencilLine, UserRound } from "lucide-react";
import { type ReactNode, useEffect, useState } from "react";

interface AvailabilityResult {
  available: boolean;
  name?: string;
}

interface UserNameGateProps {
  children: (userName: string) => ReactNode;
  checkNameAvailable: (name: string) => Promise<boolean | AvailabilityResult>;
  registerName: (name: string) => Promise<unknown>;
  storageKey?: string;
}

export function UserNameGate({
  children,
  checkNameAvailable,
  registerName,
  storageKey = "609-reading-room:user-name",
}: UserNameGateProps) {
  const [userName, setUserName] = useState<string | null>(null);
  const [draftName, setDraftName] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [hydrated, setHydrated] = useState(false);
  const [isEditorOpen, setIsEditorOpen] = useState(false);
  const [isSaving, setIsSaving] = useState(false);

  useEffect(() => {
    const storedName = window.localStorage.getItem(storageKey);
    const normalized = normalizeUserName(storedName ?? "");

    if (normalized) {
      setUserName(normalized);
      setDraftName(normalized);
      setIsEditorOpen(false);
    } else {
      setIsEditorOpen(true);
    }

    setHydrated(true);
  }, [storageKey]);

  async function handleSaveName() {
    const normalized = normalizeUserName(draftName);
    setError(null);

    if (!normalized) {
      setError("请先写上你的姓名。");
      return;
    }

    if (userName && normalized === userName) {
      setIsEditorOpen(false);
      return;
    }

    setIsSaving(true);

    try {
      const availability = await checkNameAvailable(normalized);
      const available = typeof availability === "boolean" ? availability : availability.available;

      if (!available) {
        throw new Error("这个姓名已经有人在用了，请换一个。");
      }

      await registerName(normalized);
      window.localStorage.setItem(storageKey, normalized);
      setUserName(normalized);
      setDraftName(normalized);
      setIsEditorOpen(false);
    } catch (saveError) {
      setError(saveError instanceof Error ? saveError.message : "姓名保存失败，请重试。");
    } finally {
      setIsSaving(false);
    }
  }

  function openNameEditor() {
    setDraftName(userName ?? "");
    setError(null);
    setIsEditorOpen(true);
  }

  function closeNameEditor() {
    if (userName) {
      setDraftName(userName);
      setError(null);
      setIsEditorOpen(false);
    }
  }

  if (!hydrated) {
    return (
      <section className="rounded-[32px] border border-line bg-panel p-6 shadow-card sm:p-8">
        <div className="flex items-center gap-3 text-sm text-stone-600">
          <LoaderCircle className="h-4 w-4 animate-spin" />
          <span>正在准备姓名页面…</span>
        </div>
      </section>
    );
  }

  return (
    <div className="space-y-5">
      {userName ? (
        <div className="flex justify-end">
          <button
            className="inline-flex items-center gap-3 rounded-full border border-line bg-panel px-4 py-2 text-sm text-stone-600 shadow-card transition hover:border-ink hover:text-ink"
            type="button"
            onClick={openNameEditor}
          >
            <span className="inline-flex h-8 w-8 items-center justify-center rounded-full bg-canvas text-ink">
              <UserRound className="h-4 w-4" />
            </span>
            <span>
              姓名
              <span className="ml-1 font-medium text-ink">{userName}</span>
            </span>
            <span className="inline-flex items-center gap-1 text-xs uppercase tracking-[0.18em] text-stone-500">
              <PencilLine className="h-3.5 w-3.5" />
              修改
            </span>
          </button>
        </div>
      ) : null}

      {userName ? children(userName) : null}

      {isEditorOpen ? (
        userName ? (
          <div className="fixed inset-0 z-50 flex items-center justify-center bg-ink/25 p-4 backdrop-blur-sm">
            <section className="w-full max-w-lg rounded-[32px] border border-line bg-panel p-6 shadow-card sm:p-8">
              <div className="max-w-2xl">
                <h2 className="text-3xl font-semibold tracking-tight text-ink">修改姓名</h2>
                <p className="mt-3 text-sm leading-7 text-stone-600 sm:text-base">
                  姓名将显示在打印队列中，便于取件。保存后会回到当前进度。
                </p>
              </div>

              <NameInput
                draftName={draftName}
                isSaving={isSaving}
                onChange={setDraftName}
                onSubmit={() => void handleSaveName()}
              />

              {error ? <p className="mt-3 text-sm text-danger">{error}</p> : null}

              <div className="mt-6 flex flex-col gap-3 sm:flex-row sm:items-center">
                <button
                  className="rounded-full border border-line px-4 py-3 text-sm font-medium text-ink transition hover:border-ink"
                  disabled={isSaving}
                  type="button"
                  onClick={closeNameEditor}
                >
                  取消
                </button>

                <button
                  className="inline-flex items-center justify-center rounded-full border border-ink bg-ink px-5 py-3 text-sm font-medium text-white transition hover:bg-stone-800 disabled:cursor-not-allowed disabled:border-stone-300 disabled:bg-stone-300"
                  disabled={isSaving}
                  type="button"
                  onClick={() => void handleSaveName()}
                >
                  {isSaving ? <LoaderCircle className="mr-2 h-4 w-4 animate-spin" /> : null}
                  保存姓名
                </button>
              </div>
            </section>
          </div>
        ) : (
          <section className="rounded-[32px] border border-line bg-panel p-6 shadow-card sm:p-8">
            <div className="inline-flex items-center gap-3 rounded-full border border-line bg-canvas/70 px-4 py-2 text-sm text-stone-600">
              <span className="inline-flex h-7 w-7 items-center justify-center rounded-full bg-ink text-xs font-semibold text-white">
                1
              </span>
              <span>填姓名</span>
            </div>

            <div className="mt-4 max-w-2xl">
              <h2 className="text-3xl font-semibold tracking-tight text-ink">怎么称呼您？</h2>
              <p className="mt-3 text-sm leading-7 text-stone-600 sm:text-base">
                姓名将显示在打印队列中，便于取件。
              </p>
            </div>

            <NameInput
              draftName={draftName}
              isSaving={isSaving}
              onChange={setDraftName}
              onSubmit={() => void handleSaveName()}
            />

            {error ? <p className="mt-3 text-sm text-danger">{error}</p> : null}

            <div className="mt-6 flex flex-col gap-3 sm:flex-row sm:items-center">
              <button
                className="inline-flex items-center justify-center rounded-full border border-ink bg-ink px-5 py-3 text-sm font-medium text-white transition hover:bg-stone-800 disabled:cursor-not-allowed disabled:border-stone-300 disabled:bg-stone-300"
                disabled={isSaving}
                type="button"
                onClick={() => void handleSaveName()}
              >
                {isSaving ? <LoaderCircle className="mr-2 h-4 w-4 animate-spin" /> : null}
                下一步
              </button>
            </div>
          </section>
        )
      ) : null}
    </div>
  );
}

function NameInput({
  draftName,
  isSaving,
  onChange,
  onSubmit,
}: {
  draftName: string;
  isSaving: boolean;
  onChange: (value: string) => void;
  onSubmit: () => void;
}) {
  return (
    <label className="mt-6 block max-w-xl">
      <span className="mb-2 block text-sm font-medium text-ink">姓名</span>
      <input
        autoFocus
        className="w-full rounded-[24px] border border-line bg-canvas/70 px-4 py-4 text-lg text-ink outline-none transition focus:border-ink"
        disabled={isSaving}
        placeholder="例如：张三"
        value={draftName}
        onChange={(event) => onChange(event.target.value)}
        onKeyDown={(event) => {
          if (event.key === "Enter") {
            event.preventDefault();
            onSubmit();
          }
        }}
      />
    </label>
  );
}

function normalizeUserName(value: string): string {
  return value.trim().replace(/\s+/g, " ");
}
