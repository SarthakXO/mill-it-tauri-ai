import { useEffect, useId, useRef, useState } from "react";
import type { TableBlock } from "../../../types/chat";

interface CsvExportDialogProps {
  table: TableBlock;
  onCancel: () => void;
  onExport: (columnKeys: string[]) => void;
}

export function CsvExportDialog({ table, onCancel, onExport }: CsvExportDialogProps) {
  const [selectedKeys, setSelectedKeys] = useState(() => new Set(table.columns.map((column) => column.key)));
  const titleId = useId();
  const cancelButtonRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    cancelButtonRef.current?.focus();
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") onCancel();
    };
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [onCancel]);

  const toggleColumn = (key: string) => {
    setSelectedKeys((current) => {
      const next = new Set(current);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  };
  const selectedInOriginalOrder = table.columns.filter((column) => selectedKeys.has(column.key)).map((column) => column.key);

  return (
    <div className="fixed inset-0 z-50 grid place-items-center bg-black/55 p-4" role="presentation" onMouseDown={(event) => {
      if (event.target === event.currentTarget) onCancel();
    }}>
      <div role="dialog" aria-modal="true" aria-labelledby={titleId} className="card-shadow w-full max-w-md rounded-xl border border-line bg-panel-elevated text-ink-950">
        <div className="border-b border-line px-5 py-4">
          <h2 id={titleId} className="text-base font-semibold">Export {table.title}</h2>
          <p className="mt-1 text-xs text-ink-500">Choose the columns to include in the CSV file.</p>
        </div>
        <div className="flex items-center gap-2 border-b border-line px-5 py-3">
          <button type="button" onClick={() => setSelectedKeys(new Set(table.columns.map((column) => column.key)))} className="rounded-md px-2 py-1 text-xs font-semibold text-brand-600 hover:bg-brand-50 focus-visible:outline-2 focus-visible:outline-brand-600">Select all</button>
          <button type="button" onClick={() => setSelectedKeys(new Set())} className="rounded-md px-2 py-1 text-xs font-semibold text-ink-600 hover:bg-brand-50 focus-visible:outline-2 focus-visible:outline-brand-600">Clear all</button>
          <span className="ml-auto text-[0.6875rem] text-ink-500">{selectedKeys.size} selected</span>
        </div>
        <div className="chat-scrollbar max-h-72 overflow-y-auto px-5 py-3">
          {table.columns.map((column) => (
            <label key={column.key} className="flex items-center gap-3 rounded-md px-2 py-2 text-sm text-ink-800 hover:bg-brand-50">
              <input type="checkbox" checked={selectedKeys.has(column.key)} onChange={() => toggleColumn(column.key)} className="size-4 accent-[var(--accent)]" />
              <span>{column.label}</span>
            </label>
          ))}
        </div>
        <div className="flex justify-end gap-2 border-t border-line px-5 py-4">
          <button ref={cancelButtonRef} type="button" onClick={onCancel} className="rounded-md border border-line bg-panel px-3 py-2 text-xs font-semibold text-ink-600 hover:bg-brand-50 focus-visible:outline-2 focus-visible:outline-brand-600">Cancel</button>
          <button type="button" disabled={selectedKeys.size === 0} onClick={() => onExport(selectedInOriginalOrder)} className="rounded-md bg-brand-700 px-3 py-2 text-xs font-semibold text-white hover:bg-brand-600 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-brand-600 disabled:cursor-not-allowed disabled:opacity-45">Export CSV</button>
        </div>
      </div>
    </div>
  );
}
