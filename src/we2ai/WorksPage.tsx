import { useCallback, useEffect, useRef, useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import {
  Download,
  FolderOpen,
  Play,
  RefreshCw,
  Star,
  Trash2,
  X,
} from "lucide-react";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import {
  Dialog,
  DialogContent,
  DialogClose,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { we2aiApi, type We2aiStudioWork } from "./api";
import { studioButtonClass, studioMutedClass } from "./StudioMediaParts";
import { getStudioMediaStrings } from "./studioMediaStrings";
import { studioErrorMessage } from "./studioErrors";
import {
  formatWe2aiString,
  type We2aiLanguage,
  type We2aiStrings,
} from "./strings";

interface WorksPageProps {
  t: We2aiStrings;
  language: We2aiLanguage;
  refreshTick: number;
}
type MediaWork = We2aiStudioWork & { kind: "image" | "video" };
const PAGE_SIZE = 20;

export function WorksPage({ t, language, refreshTick }: WorksPageProps) {
  const ms = getStudioMediaStrings(language);
  const [works, setWorks] = useState<MediaWork[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [kind, setKind] = useState<"image" | "video" | "">("");
  const [favoritesOnly, setFavoritesOnly] = useState(false);
  const [limit, setLimit] = useState(PAGE_SIZE);
  const [deleting, setDeleting] = useState<MediaWork | null>(null);
  const [preview, setPreview] = useState<MediaWork | null>(null);
  const [failedPreviews, setFailedPreviews] = useState<Set<string>>(new Set());
  const requestSeq = useRef(0);
  const currentPreview = works.find((work) => work.id === preview?.id) ?? null;

  const load = useCallback(
    async (silent = false) => {
      const seq = ++requestSeq.current;
      if (!silent) setLoading(true);
      try {
        const next = await we2aiApi.playgroundMediaList();
        if (requestSeq.current !== seq) return;
        setWorks(
          next.filter(
            (work): work is MediaWork =>
              work.kind === "image" || work.kind === "video",
          ),
        );
        setError(null);
      } catch {
        if (requestSeq.current === seq) setError(ms.worksLoadError);
      } finally {
        if (requestSeq.current === seq) setLoading(false);
      }
    },
    [ms.worksLoadError],
  );

  useEffect(() => {
    void load();
    return () => {
      requestSeq.current += 1;
    };
  }, [load, refreshTick]);
  const hasPending = works.some((w) => w.status === "processing");
  useEffect(() => {
    if (!hasPending) return;
    const timer = window.setInterval(() => void load(true), 3000);
    return () => window.clearInterval(timer);
  }, [hasPending, load]);

  const action = async (fn: () => Promise<unknown>) => {
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      await fn();
    } catch {
      setError(ms.fileActionFailed);
    } finally {
      setBusy(false);
    }
  };
  const save = (path: string) =>
    action(async () => {
      const dest = await we2aiApi.playgroundMediaSaveAs(path);
      if (dest) setNotice(formatWe2aiString(ms.savedTo, { path: dest }));
    });
  const change = (work: We2aiStudioWork, favorite: boolean | null) =>
    action(async () => {
      await we2aiApi.playgroundWorkUpdate(work.id, favorite);
      setDeleting(null);
      await load(true);
    });
  const status = (work: We2aiStudioWork) => {
    if (work.status === "processing")
      return (
        ms.worksProcessing +
        (work.progress === null ? "" : ` ${work.progress}%`)
      );
    if (work.status === "cancelled") return ms.worksCancelled;
    if (work.status === "interrupted") return ms.worksInterrupted;
    if (work.status === "failed")
      return work.message
        ? studioErrorMessage(t, ms, work.message, ms.worksFailed)
        : ms.worksFailed;
    return ms.worksSuccess;
  };
  const typeLabels = {
    video: ms.worksVideo,
    image: ms.worksImage,
  };
  const filtered = works.filter(
    (w) => (!kind || w.kind === kind) && (!favoritesOnly || w.favorite),
  );
  const renderContent = (work: MediaWork, full: boolean) => (
    <>
      {(full ? work.files : work.files.slice(0, 1)).map((file) => (
        <div key={file.path} className="space-y-2">
          {failedPreviews.has(file.path) ? (
            <p className="text-sm">{ms.worksPreviewFailed}</p>
          ) : work.kind === "video" ? (
            <video
              src={convertFileSrc(file.path)}
              controls
              preload="metadata"
              className="max-h-96 w-full rounded-lg bg-black"
              onError={() =>
                setFailedPreviews((prev) => new Set(prev).add(file.path))
              }
              data-testid="we2ai-work-player"
            />
          ) : work.kind === "image" ? (
            <img
              src={convertFileSrc(file.path)}
              alt={work.name}
              loading="lazy"
              className="max-h-96 w-full rounded-lg object-contain"
              onError={() =>
                setFailedPreviews((prev) => new Set(prev).add(file.path))
              }
              data-testid="we2ai-work-image"
            />
          ) : null}
          {full && (
            <Button
              size="sm"
              variant="outline"
              disabled={busy}
              onClick={() => void save(file.path)}
            >
              {ms.worksDownload} · {(file.bytes / 1024 / 1024).toFixed(1)} MB
            </Button>
          )}
        </div>
      ))}
    </>
  );

  return (
    <Card className="we2ai-panel">
      <CardHeader className="flex-row items-center justify-between gap-4 border-b-2 border-[var(--we2ai-ink)]">
        <div>
          <CardTitle className="we2ai-heading text-xl">
            {ms.worksTitle}
          </CardTitle>
          <CardDescription>{ms.worksDescription}</CardDescription>
        </div>
        <Button
          size="sm"
          variant="outline"
          disabled={loading || busy}
          className={studioButtonClass.plain}
          onClick={() => void load()}
          data-testid="we2ai-works-refresh"
        >
          <RefreshCw className="h-3.5 w-3.5" />
          {t.refresh}
        </Button>
      </CardHeader>
      <CardContent className="space-y-4 pt-6">
        <div
          className="flex flex-wrap gap-2"
          role="group"
          aria-label={ms.worksTitle}
        >
          {(
            [
              ["", ms.worksAll],
              ["video", ms.worksVideo],
              ["image", ms.worksImage],
            ] as const
          ).map(([value, label]) => (
            <Button
              key={value}
              variant="outline"
              size="sm"
              aria-pressed={kind === value}
              className={
                kind === value ? studioButtonClass.ink : studioButtonClass.plain
              }
              onClick={() => {
                setKind(value);
                setLimit(PAGE_SIZE);
              }}
            >
              {label}
            </Button>
          ))}
          <Button
            variant="outline"
            size="sm"
            aria-pressed={favoritesOnly}
            className={
              favoritesOnly ? studioButtonClass.ink : studioButtonClass.plain
            }
            onClick={() => {
              setFavoritesOnly((v) => !v);
              setLimit(PAGE_SIZE);
            }}
          >
            <Star className="h-3.5 w-3.5" />
            {ms.worksFavorites}
          </Button>
        </div>
        {error && (
          <p role="alert" className="text-sm text-[var(--we2ai-danger)]">
            {error}
          </p>
        )}
        {notice && (
          <p role="status" className="text-sm">
            {notice}
          </p>
        )}
        {loading && (
          <p className="text-sm" data-testid="we2ai-works-loading">
            {ms.worksLoading}
          </p>
        )}
        {!loading && !error && filtered.length === 0 && (
          <p className={`text-sm ${studioMutedClass}`}>
            {works.length ? ms.worksNoMatch : ms.worksEmpty}
          </p>
        )}
        <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-3">
          {filtered.slice(0, limit).map((work) => (
            <article
              key={work.id}
              className="flex flex-col gap-3 rounded-lg border-2 border-[var(--we2ai-ink)] p-4"
              data-testid="we2ai-work"
            >
              {renderContent(work, false)}
              <button
                type="button"
                className="break-words text-left text-sm font-bold underline"
                onClick={() => setPreview(work)}
              >
                {work.name}
              </button>
              <p className={`text-xs ${studioMutedClass}`}>
                {typeLabels[work.kind]} ·{" "}
                {new Date(work.savedAtMs).toLocaleString(
                  language === "zh" ? "zh-CN" : "en-US",
                )}{" "}
                {work.model && `· ${work.model}`}
              </p>
              <p className="text-sm" data-testid="we2ai-work-status">
                {status(work)}
              </p>
              <div className="mt-auto flex flex-wrap gap-2">
                {work.status === "success" && (
                  <>
                    <Button
                      size="sm"
                      variant="outline"
                      className={studioButtonClass.plain}
                      disabled={busy}
                      onClick={() => void change(work, !work.favorite)}
                      aria-label={
                        work.favorite ? ms.worksUnfavorite : ms.worksFavorite
                      }
                    >
                      <Star
                        className={`h-3.5 w-3.5 ${work.favorite ? "fill-current" : ""}`}
                      />
                    </Button>
                    {work.files.length === 1 && (
                      <Button
                        size="sm"
                        variant="outline"
                        className={studioButtonClass.plain}
                        disabled={busy}
                        onClick={() => void save(work.files[0].path)}
                      >
                        <Download className="h-3.5 w-3.5" />
                        {ms.worksDownload}
                      </Button>
                    )}
                    {work.files.length > 1 && (
                      <Button
                        size="sm"
                        variant="outline"
                        onClick={() => setPreview(work)}
                      >
                        {ms.worksDownload} ({work.files.length})
                      </Button>
                    )}
                  </>
                )}
                {work.files[0] && (
                  <>
                    <Button
                      size="sm"
                      variant="outline"
                      className={studioButtonClass.plain}
                      disabled={busy}
                      onClick={() =>
                        void action(() =>
                          we2aiApi.playgroundMediaOpen(
                            work.files[0].path,
                            false,
                          ),
                        )
                      }
                    >
                      <Play className="h-3.5 w-3.5" />
                      {ms.openFile}
                    </Button>
                    <Button
                      size="sm"
                      variant="outline"
                      className={studioButtonClass.plain}
                      disabled={busy}
                      onClick={() =>
                        void action(() =>
                          we2aiApi.playgroundMediaOpen(
                            work.files[0].path,
                            true,
                          ),
                        )
                      }
                    >
                      <FolderOpen className="h-3.5 w-3.5" />
                      {ms.revealFile}
                    </Button>
                  </>
                )}
                {work.status !== "processing" && (
                  <Button
                    size="sm"
                    variant="outline"
                    className={studioButtonClass.plain}
                    disabled={busy}
                    onClick={() => setDeleting(work)}
                  >
                    <Trash2 className="h-3.5 w-3.5" />
                    {ms.worksDelete}
                  </Button>
                )}
              </div>
            </article>
          ))}
        </div>
        {filtered.length > limit && (
          <Button
            variant="outline"
            onClick={() => setLimit((n) => n + PAGE_SIZE)}
          >
            {ms.worksMore}
          </Button>
        )}
      </CardContent>
      <Dialog
        open={preview !== null}
        onOpenChange={(open) => {
          if (!open) setPreview(null);
        }}
      >
        <DialogContent
          className="we2ai-theme max-h-[90vh] overflow-hidden sm:max-w-3xl"
          onInteractOutside={() => {
            // Allow outside dismissal for this read-only preview only.
          }}
        >
          <DialogHeader className="relative pr-16">
            <DialogTitle className="break-words">
              {currentPreview?.name}
            </DialogTitle>
            <DialogClose asChild>
              <Button
                type="button"
                variant="outline"
                size="icon"
                className={`${studioButtonClass.plain} absolute right-4 top-4`}
                aria-label={ms.worksClose}
              >
                <X className="h-4 w-4" />
              </Button>
            </DialogClose>
            <DialogDescription>
              {currentPreview && status(currentPreview)}
            </DialogDescription>
          </DialogHeader>
          <div className="min-h-0 space-y-4 overflow-y-auto p-6">
            {currentPreview && renderContent(currentPreview, true)}
          </div>
          <DialogFooter>
            <DialogClose asChild>
              <Button
                type="button"
                variant="outline"
                className={studioButtonClass.plain}
              >
                {ms.worksClose}
              </Button>
            </DialogClose>
          </DialogFooter>
        </DialogContent>
      </Dialog>
      <Dialog
        open={deleting !== null}
        onOpenChange={(open) => {
          if (!open && !busy) setDeleting(null);
        }}
      >
        <DialogContent className="we2ai-theme">
          <DialogHeader>
            <DialogTitle>{ms.worksDelete}</DialogTitle>
            <DialogDescription>{ms.worksDeleteConfirm}</DialogDescription>
          </DialogHeader>
          <p className="break-words">{deleting?.name}</p>
          <DialogFooter>
            <Button
              variant="outline"
              disabled={busy}
              onClick={() => setDeleting(null)}
            >
              {ms.worksCancel}
            </Button>
            <Button
              variant="destructive"
              disabled={busy}
              onClick={() => deleting && void change(deleting, null)}
            >
              {ms.worksDelete}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </Card>
  );
}
