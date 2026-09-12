import { useState } from "react";
import {
  FolderOpen,
  EyeOff,
  Sparkles,
  ShieldCheck,
  X,
} from "lucide-react";
import type { ScanItemDto } from "@/types";
import { getBackend } from "@/adapters";
import { useUnknownWorkflowStore } from "@/stores/unknownWorkflowStore";
import { useUiStore } from "@/stores/uiStore";
import { useScanStore } from "@/stores/scanStore";
import { useAiStore } from "@/stores/aiStore";

/**
 * The Unknown item's disposition toolkit (SPEC §25): Open Folder / Ignore /
 * Protect. Decisions route through a confirmation dialog that spells out
 * what the choice means.
 *
 * Layout: action rail above the six-question block inside the Detail Panel
 * — actions live *with* the item context, not in table rows (keeps the
 * eight-column density intact and gives each action room to explain
 * itself).
 */
export function UnknownActions({ item }: { item: ScanItemDto }) {
  const applying = useUnknownWorkflowStore((s) => s.applying.has(item.id));
  const setPage = useUiStore((s) => s.setPage);
  const generation = useScanStore((s) => s.generation);
  const setReturnContext = useAiStore((s) => s.setReturnContext);

  const [confirming, setConfirming] = useState<null | "ignore" | "protect">(null);
  const [openErr, setOpenErr] = useState<string | null>(null);

  const openFolder = async () => {
    setOpenErr(null);
    try {
      await getBackend().openFolder(item.id);
    } catch (e) {
      setOpenErr(e instanceof Error ? e.message : "无法打开文件夹");
    }
  };

  const busy = applying;

  return (
    <div className="unknown-actions">
      <div className="action-rail">
        <button className="btn small" onClick={() => void openFolder()} disabled={busy}>
          <FolderOpen size={12} /> 打开文件夹
        </button>
        <button
          className="btn small"
          onClick={() => setConfirming("ignore")}
          disabled={busy}
          title="之后的扫描不再报告此文件夹"
        >
          <EyeOff size={12} /> 忽略
        </button>
        <button
          className="btn small"
          onClick={() => setConfirming("protect")}
          disabled={busy}
          title="永不清理，始终受保护"
        >
          <ShieldCheck size={12} /> 保护
        </button>
        <button
          className="btn small ghost"
          onClick={() => {
            setReturnContext({
              sourcePage: "unknown",
              sourceView: item.risk === "review" ? "review" : "unknown",
              itemIds: [item.id],
              scanGeneration: generation,
            });
            setPage("ai-review");
          }}
          disabled={busy}
          title="仅将当前条目带入联网 AI 研判候选，不会自动发送"
        >
          <Sparkles size={12} /> 使用 AI 辅助判断
        </button>
      </div>

      {openErr && <div className="unknown-err">{openErr}</div>}

      {confirming && (
        <DispositionConfirm
          item={item}
          kind={confirming}
          onCancel={() => setConfirming(null)}
        />
      )}
    </div>
  );
}

function DispositionConfirm({
  item,
  kind,
  onCancel,
}: {
  item: ScanItemDto;
  /** UI state word; maps onto the wire disposition ("ignore"/"protect"). */
  kind: "ignore" | "protect";
  onCancel: () => void;
}) {
  const setDisposition = useUnknownWorkflowStore((s) => s.setDisposition);
  const applying = useUnknownWorkflowStore((s) => s.applying.has(item.id));
  const disposition = kind === "protect" ? "protect" : "ignore";

  return (
    <div className="confirm-note protected" style={{ flexDirection: "column", gap: 8 }}>
      <div style={{ display: "flex", gap: 10, alignItems: "flex-start" }}>
        {kind === "protect" ? (
          <ShieldCheck size={15} style={{ marginTop: 1, flex: "none" }} />
        ) : (
          <EyeOff size={15} style={{ marginTop: 1, flex: "none" }} />
        )}
        <span>
          {kind === "protect" ? (
            <>
              <b>将此文件夹设为受保护？</b>
              它会移入「受保护」类别——任何清理中 DevResidue 都不会把它列入删除计划。之后只有你能撤销此设置。
            </>
          ) : (
            <>
              <b>忽略此文件夹？</b>
              之后的扫描将不再报告它。不会删除任何数据——文件保持原位，只是不再出现在报告中。
            </>
          )}
        </span>
      </div>
      <div className="mono-path">{item.path}</div>
      <div style={{ display: "flex", gap: 8, justifyContent: "flex-end", width: "100%" }}>
        <button className="btn small ghost" onClick={onCancel}>
          <X size={11} /> 取消
        </button>
        <button
          className={`btn small ${kind === "protect" ? "primary" : ""}`}
          disabled={applying}
          onClick={() => void setDisposition(item.id, disposition)}
        >
          {applying ? "应用中…" : kind === "protect" ? "设为受保护" : "确认忽略"}
        </button>
      </div>
    </div>
  );
}
