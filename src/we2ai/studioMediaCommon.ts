/**
 * 图片 / 视频面板的非 UI 公共逻辑（功能 27，P2/P3）。
 */

import { useCallback, useEffect, useRef, useState } from "react";
import {
  isWe2aiApiError,
  we2aiApi,
  type We2aiMediaModel,
  type We2aiMediaModels,
  type We2aiReferenceImage,
} from "./api";
import { SESSION_ERROR_CODES } from "./studioErrors";

/** 参考图大小上限（字节），与 Rust `MEDIA_MAX_REFERENCE_BYTES` 一致。 */
export const REFERENCE_MAX_BYTES = 8 * 1024 * 1024;
export const REFERENCE_TOTAL_MAX_BYTES = 32 * 1024 * 1024;

const REFERENCE_MIMES = ["image/png", "image/jpeg", "image/webp"] as const;

export function newRequestId(): string {
  return typeof crypto !== "undefined" && "randomUUID" in crypto
    ? crypto.randomUUID()
    : `${Date.now()}-${Math.random().toString(36).slice(2)}`;
}

export type ReferenceReadResult =
  | { ok: true; reference: We2aiReferenceImage; previewUrl: string }
  | { ok: false; reason: "type" | "size" | "read" };

/** 读本地图片为 base64（只做大小与类型预检，最终校验在 Rust 侧按字节头进行）。 */
export function readReferenceFile(file: File): Promise<ReferenceReadResult> {
  if (!(REFERENCE_MIMES as readonly string[]).includes(file.type)) {
    return Promise.resolve({ ok: false, reason: "type" });
  }
  if (file.size > REFERENCE_MAX_BYTES) {
    return Promise.resolve({ ok: false, reason: "size" });
  }
  return new Promise((resolve) => {
    const reader = new FileReader();
    reader.onerror = () => resolve({ ok: false, reason: "read" });
    reader.onload = () => {
      const result = typeof reader.result === "string" ? reader.result : "";
      const comma = result.indexOf(",");
      if (!result.startsWith("data:") || comma < 0) {
        resolve({ ok: false, reason: "read" });
        return;
      }
      resolve({
        ok: true,
        reference: {
          mime: file.type as We2aiReferenceImage["mime"],
          dataBase64: result.slice(comma + 1),
        },
        previewUrl: result,
      });
    };
    reader.readAsDataURL(file);
  });
}

/** 字符串的 UTF-8 字节数（Grok 视频提示词上限按字节计）。 */
export function utf8Bytes(text: string): number {
  return new TextEncoder().encode(text).length;
}

/**
 * 加载某个 Key 下的图片 / 视频模型。序号保护乱序返回；父组件以 `key={keyId}`
 * 重挂载面板，所以这里不处理换 Key。`refreshTick` 变化时静默重拉（切回 Tab、窗口
 * 聚焦、手动刷新）：不显示加载态，已选模型仍在列表里就保留，失败时保留旧列表。
 */
export function useMediaModels(
  keyId: number,
  kind: "image" | "video",
  fallbackError: string,
  onSessionMaybeEnded: () => void,
  refreshTick = 0,
) {
  const [models, setModels] = useState<We2aiMediaModels | null>(null);
  const [model, setModel] = useState<We2aiMediaModel | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const seqRef = useRef(0);
  const sessionEndedRef = useRef(onSessionMaybeEnded);
  sessionEndedRef.current = onSessionMaybeEnded;

  const load = useCallback(
    async (silent = false) => {
      const seq = ++seqRef.current;
      if (!silent) {
        setLoading(true);
        setError(null);
      }
      try {
        const result = await we2aiApi.playgroundMediaModels(keyId, kind);
        if (seq !== seqRef.current) return;
        // 先在 try 内取出列表：更新函数在渲染阶段执行，里面抛错不会被这里捕获。
        const list = result.models;
        setModels(result);
        setModel(
          (prev) => list.find((m) => m.id === prev?.id) ?? list[0] ?? null,
        );
      } catch (e) {
        if (seq !== seqRef.current) return;
        if (!silent) {
          setModels(null);
          setModel(null);
          setError(isWe2aiApiError(e) ? e.message : fallbackError);
        }
        if (isWe2aiApiError(e) && SESSION_ERROR_CODES.has(e.code)) {
          sessionEndedRef.current();
        }
      } finally {
        // 静默请求可能顶替了仍在加载的前台请求，收尾不按 silent 区分。
        if (seq === seqRef.current) setLoading(false);
      }
    },
    [keyId, kind, fallbackError],
  );

  useEffect(() => {
    void load();
    return () => {
      seqRef.current += 1;
    };
  }, [load]);

  const lastTickRef = useRef(refreshTick);
  useEffect(() => {
    if (refreshTick === lastTickRef.current) return;
    lastTickRef.current = refreshTick;
    void load(true);
  }, [refreshTick, load]);

  const selectModel = useCallback(
    (id: string) => {
      setModel(models?.models.find((m) => m.id === id) ?? null);
    },
    [models],
  );

  return {
    models,
    model,
    selectModel,
    loading,
    error,
    reload: () => load(),
  };
}

export interface CompileResult {
  ok: boolean;
  code?: string | null;
  message?: string;
}

/**
 * 用文本模型把模板草稿润色成最终提示词（流式写回）。复用 `playgroundChat` 与增量
 * 事件；每次调用先建立监听再 invoke，保证早期 delta 不丢。
 */
export function useStudioCompile(keyId: number, textModel: string | null) {
  const [compiling, setCompiling] = useState(false);
  const activeRef = useRef<string | null>(null);

  const cancel = useCallback(() => {
    const id = activeRef.current;
    if (id) void we2aiApi.playgroundCancel(id).catch(() => {});
  }, []);

  useEffect(() => cancel, [cancel]);

  const compile = useCallback(
    async (
      system: string,
      user: string,
      onDelta: (delta: string) => void,
    ): Promise<CompileResult> => {
      if (!textModel || activeRef.current) return { ok: false };
      const requestId = newRequestId();
      activeRef.current = requestId;
      setCompiling(true);
      let unlisten: (() => void) | null = null;
      try {
        unlisten = await we2aiApi.onPlaygroundChunk((event) => {
          if (event.requestId === requestId) onDelta(event.delta);
        });
        await we2aiApi.playgroundChat({
          requestId,
          keyId,
          model: textModel,
          system,
          messages: [{ role: "user", content: user }],
          temperature: 0.4,
          maxTokens: 1024,
        });
        return { ok: true };
      } catch (e) {
        return isWe2aiApiError(e)
          ? { ok: false, code: e.code, message: e.message }
          : {
              ok: false,
              code: null,
              message: e instanceof Error ? e.message : "",
            };
      } finally {
        unlisten?.();
        if (activeRef.current === requestId) activeRef.current = null;
        setCompiling(false);
      }
    },
    [keyId, textModel],
  );

  return { compiling, compile, cancel };
}
