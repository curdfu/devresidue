import type { RiskLevel, SessionItemStatus } from "@/types";
import { riskMeta } from "@/utils/format";
import { Radar } from "lucide-react";
import { useScanStore } from "@/stores/scanStore";

export function RiskBadge({ risk }: { risk: RiskLevel }) {
  const meta = riskMeta(risk);
  return (
    <span className={`badge ${meta.className}`} title={meta.hint}>
      {meta.label}
    </span>
  );
}

export function StatusBadge({ status }: { status: SessionItemStatus }) {
  const label =
    status === "ok"
      ? "已清理"
      : status === "would"
        ? "预演"
        : status === "skipped"
          ? "已跳过"
          : "失败";
  return <span className={`badge status-${status}`}>{label}</span>;
}

export function EmptyScanState() {
  const startScan = useScanStore((s) => s.startScan);
  return (
    <div className="empty">
      <Radar size={30} strokeWidth={1.4} />
      <h3>尚未扫描</h3>
      <p>
        运行一次扫描，即可查看 AI Agent、开发缓存与构建产物分别占用了多少空间——
        每一项都附带风险等级与删除影响说明。
      </p>
      <button className="btn primary" onClick={() => void startScan({ kind: "default" })}>
        <Radar size={13} /> 开始扫描
      </button>
    </div>
  );
}
