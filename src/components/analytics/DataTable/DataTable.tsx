import { useEffect, useRef, useState } from "react";
import type { TableBlock, TableCellValue, TableColumn } from "../../../types/chat";

interface DataTableProps {
  table: TableBlock;
}

export function tableToTsv(table: TableBlock): string {
  const sanitize = (value: TableCellValue): string =>
    value === null ? "" : String(value).replace(/[\t\r\n]+/g, " ");
  return [
    table.columns.map((column) => sanitize(column.label)).join("\t"),
    ...table.rows.map((row) =>
      table.columns.map((column) => sanitize(row[column.key] ?? null)).join("\t"),
    ),
  ].join("\n");
}

async function copyText(value: string): Promise<void> {
  if (navigator.clipboard?.writeText) {
    await navigator.clipboard.writeText(value);
    return;
  }

  const textarea = document.createElement("textarea");
  textarea.value = value;
  textarea.style.position = "fixed";
  textarea.style.opacity = "0";
  document.body.appendChild(textarea);
  textarea.select();
  const copied = document.execCommand("copy");
  textarea.remove();
  if (!copied) throw new Error("Clipboard copy was not available");
}

export function DataTable({ table }: DataTableProps) {
  const [copyState, setCopyState] = useState<"idle" | "copied" | "failed">("idle");
  const resetTimer = useRef<number | undefined>(undefined);

  useEffect(
    () => () => {
      if (resetTimer.current !== undefined) window.clearTimeout(resetTimer.current);
    },
    [],
  );

  const copyTable = async () => {
    try {
      await copyText(tableToTsv(table));
      setCopyState("copied");
    } catch {
      setCopyState("failed");
    }
    if (resetTimer.current !== undefined) window.clearTimeout(resetTimer.current);
    resetTimer.current = window.setTimeout(() => setCopyState("idle"), 1800);
  };

  return (
    <section className="overflow-hidden rounded-xl border border-line bg-white shadow-[0_2px_10px_rgba(22,33,29,0.04)]">
      <div className="flex items-center justify-between gap-4 border-b border-line px-4 py-3">
        <div>
          <h3 className="text-sm font-semibold text-ink-950">{table.title}</h3>
          <p className="mt-0.5 text-[0.6875rem] text-ink-500">
            {table.rowCount.toLocaleString()} {table.rowCount === 1 ? "row" : "rows"}
          </p>
        </div>
        <button
          type="button"
          onClick={() => void copyTable()}
          disabled={table.columns.length === 0}
          className="min-w-17 rounded-md border border-line bg-surface px-3 py-1.5 text-xs font-semibold text-ink-600 transition hover:border-[#bdcbc2] hover:bg-brand-50 hover:text-brand-700 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-brand-600 disabled:cursor-not-allowed disabled:opacity-50"
          aria-label={`Copy ${table.title} as TSV`}
        >
          {copyState === "copied" ? "Copied" : copyState === "failed" ? "Try again" : "Copy"}
        </button>
      </div>

      {table.rows.length === 0 ? (
        <div className="px-4 py-8 text-center text-sm text-ink-500">No matching data.</div>
      ) : (
        <div className="chat-scrollbar overflow-x-auto">
          <table className="w-full min-w-max border-collapse text-left text-sm">
            <thead className="bg-surface text-xs font-semibold uppercase tracking-[0.04em] text-ink-500">
              <tr>
                {table.columns.map((column) => (
                  <th
                    key={column.key}
                    scope="col"
                    className={`whitespace-nowrap border-b border-line px-4 py-3 ${
                      isNumeric(column) ? "text-right" : "text-left"
                    }`}
                  >
                    {column.label}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody className="text-ink-800">
              {table.rows.map((row, rowIndex) => (
                <tr key={rowIndex} className="border-b border-line last:border-b-0 hover:bg-brand-50/40">
                  {table.columns.map((column) => (
                    <td
                      key={column.key}
                      className={`max-w-80 px-4 py-3 align-top ${
                        isNumeric(column) ? "text-right tabular-nums" : "text-left"
                      }`}
                    >
                      {formatCell(row[column.key] ?? null, column)}
                    </td>
                  ))}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}

      {table.truncated && (
        <div className="border-t border-amber-200 bg-amber-50 px-4 py-2.5 text-xs text-amber-900">
          Result truncated at the read-only analytics limit.
        </div>
      )}
    </section>
  );
}

function isNumeric(column: TableColumn): boolean {
  return ["number", "currency", "percentage"].includes(column.type);
}

function formatCell(value: TableCellValue, column: TableColumn): string {
  if (value === null || value === "") return "—";
  if (typeof value === "boolean") return value ? "Yes" : "No";
  if (typeof value === "number") {
    if (column.type === "currency") {
      return new Intl.NumberFormat("en-IN", {
        style: "currency",
        currency: "INR",
        maximumFractionDigits: 2,
      }).format(value);
    }
    if (column.type === "percentage") return `${value.toLocaleString("en-IN")}%`;
    return value.toLocaleString("en-IN", { maximumFractionDigits: 2 });
  }
  if (column.type === "datetime") {
    const date = new Date(value);
    if (!Number.isNaN(date.getTime())) {
      return new Intl.DateTimeFormat("en-IN", {
        dateStyle: "medium",
        timeStyle: "short",
        timeZone: "Asia/Kolkata",
      }).format(date);
    }
  }
  return value;
}
