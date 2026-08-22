import { memo, useEffect, useState } from "react";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { Icon } from "../Icon";
import { isImagePath } from "../../state/openFile";
import { isWallpaperVideo } from "../../state/themes";
import { resolveWallpaperSrc } from "../../state/wallpaperSrc";
import { useT } from "../../i18n";

export const CanvasFileCard = memo(function CanvasFileCard({
  path,
  name,
  selected,
  marked,
  onSelect,
  onDoubleClick,
  onPointerDownDrag,
  onResizePointerDown,
  onCardContextMenu,
}: {
  path: string;
  name: string;
  selected: boolean;
  marked?: boolean;
  onSelect: (e: React.MouseEvent) => void;
  onDoubleClick: () => void;
  onPointerDownDrag: (e: React.PointerEvent<HTMLDivElement>) => void;
  onResizePointerDown?: (
    e: React.PointerEvent<HTMLDivElement>,
    edge: "e" | "s" | "se"
  ) => void;
  onCardContextMenu?: (e: React.MouseEvent) => void;
}) {
  const t = useT();
  const image = isImagePath(path);
  const video = isWallpaperVideo(path);
  const [text, setText] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [videoSrc, setVideoSrc] = useState<string | null>(null);

  useEffect(() => {
    if (image || video) return;
    let cancelled = false;
    setText(null);
    setError(null);
    invoke<string>("fs_read", { path })
      .then((content) => {
        if (cancelled) return;
        setText(content.slice(0, 8000));
      })
      .catch((e) => {
        if (cancelled) return;
        setError(typeof e === "string" ? e : t("canvas.filePreviewFailed"));
      });
    return () => {
      cancelled = true;
    };
  }, [path, image, video, t]);

  useEffect(() => {
    if (!video) {
      setVideoSrc(null);
      return;
    }
    let cancelled = false;
    let revoke: (() => void) | undefined;
    setError(null);
    setVideoSrc(null);
    void resolveWallpaperSrc(path, undefined, true)
      .then((resolved) => {
        if (cancelled) {
          resolved?.revoke?.();
          return;
        }
        if (!resolved) {
          setError(t("canvas.filePreviewFailed"));
          return;
        }
        revoke = resolved.revoke;
        setVideoSrc(resolved.url);
      })
      .catch((e) => {
        if (cancelled) return;
        setError(typeof e === "string" ? e : t("canvas.filePreviewFailed"));
      });
    return () => {
      cancelled = true;
      revoke?.();
    };
  }, [path, video, t]);

  const iconName = video ? "play" : image ? "image" : "file-text";

  return (
    <div
      className={`canvas-card expanded file${selected ? " selected" : ""}${marked ? " marked" : ""}`}
      draggable={false}
      onDragStart={(e) => e.preventDefault()}
      onPointerDown={(e) => {
        if (
          !(e.target as HTMLElement).closest(".canvas-card-title, .canvas-card-drag") ||
          (e.target as HTMLElement).closest("button, .canvas-card-body, .canvas-card-grip, .canvas-pty-resize")
        ) {
          return;
        }
        e.preventDefault();
        onPointerDownDrag(e);
      }}
      onClick={(e) => {
        e.stopPropagation();
        onSelect(e);
      }}
      onDoubleClick={(e) => {
        e.stopPropagation();
        onDoubleClick();
      }}
      onContextMenu={(e) => {
        e.preventDefault();
        e.stopPropagation();
        onCardContextMenu?.(e);
      }}
    >
      <div className="canvas-card-title canvas-card-drag">
        <Icon name={iconName} size={12} />
        <span className="canvas-card-name">{name}</span>
      </div>
      <div
        className="canvas-card-body"
        onPointerDown={(e) => {
          e.stopPropagation();
          if (e.button !== 0) return;
          const card = e.currentTarget.closest(".canvas-card") as HTMLElement | null;
          const surface = e.currentTarget.closest(".canvas-surface") as HTMLElement | null;
          card?.classList.add("selecting-text");
          surface?.classList.add("selecting");
          const clear = () => {
            card?.classList.remove("selecting-text");
            surface?.classList.remove("selecting");
            window.removeEventListener("pointerup", clear, true);
            window.removeEventListener("pointercancel", clear, true);
          };
          window.addEventListener("pointerup", clear, true);
          window.addEventListener("pointercancel", clear, true);
        }}
        onMouseDown={(e) => e.stopPropagation()}
        onWheel={(e) => e.stopPropagation()}
      >
        {image ? (
          <img src={convertFileSrc(path)} alt={name} draggable={false} />
        ) : video ? (
          error ? (
            <div className="canvas-file-error">{error}</div>
          ) : videoSrc ? (
            <video
              src={videoSrc}
              muted
              controls
              playsInline
              preload="metadata"
              draggable={false}
              onError={() => setError(t("canvas.filePreviewFailed"))}
            />
          ) : (
            <div className="canvas-file-empty">{t("common.loading")}</div>
          )
        ) : error ? (
          <div className="canvas-file-error">{error}</div>
        ) : text == null ? (
          <div className="canvas-file-empty">{t("common.loading")}</div>
        ) : (
          <pre>{text}</pre>
        )}
      </div>
      {onResizePointerDown && (
        <>
          <div
            className="canvas-card-grip canvas-card-grip-resize canvas-card-grip-r"
            onPointerDown={(e) => {
              e.preventDefault();
              e.stopPropagation();
              onResizePointerDown(e, "e");
            }}
          />
          <div
            className="canvas-card-grip canvas-card-grip-resize canvas-card-grip-b"
            onPointerDown={(e) => {
              e.preventDefault();
              e.stopPropagation();
              onResizePointerDown(e, "s");
            }}
          />
          <div
            className="canvas-pty-resize"
            onPointerDown={(e) => {
              e.preventDefault();
              e.stopPropagation();
              onResizePointerDown(e, "se");
            }}
          />
        </>
      )}
    </div>
  );
});
