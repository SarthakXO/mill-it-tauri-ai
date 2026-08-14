import type { TableBlock, TableCellValue, TableColumn } from "../../../types/chat";

export function isNumericColumn(column: TableColumn): boolean {
  return column.type === "number" || column.type === "currency" || column.type === "percentage";
}

export function isSortableColumn(column: TableColumn): boolean {
  return isNumericColumn(column) || column.type === "date" || column.type === "datetime" || column.type === "string";
}

export function formatCell(value: TableCellValue, column: TableColumn): string {
  if (value === null || value === "") return "—";
  if (typeof value === "boolean") return value ? "Yes" : "No";
  if (typeof value === "number") {
    if (column.type === "currency") {
      return new Intl.NumberFormat("en-IN", { style: "currency", currency: "INR", maximumFractionDigits: 2 }).format(value);
    }
    if (column.type === "percentage") return `${value.toLocaleString("en-IN")}%`;
    return value.toLocaleString("en-IN", { maximumFractionDigits: 2 });
  }
  if (column.type === "date" || column.type === "datetime") {
    const date = new Date(value);
    if (!Number.isNaN(date.getTime())) {
      return new Intl.DateTimeFormat("en-IN", {
        dateStyle: "medium",
        ...(column.type === "datetime" ? { timeStyle: "short" as const } : {}),
        timeZone: "Asia/Kolkata",
      }).format(date);
    }
  }
  return String(value);
}

function tsvValue(value: TableCellValue): string {
  return value === null ? "" : String(value).replace(/[\t\r\n]+/g, " ");
}

export function tableToTsv(table: TableBlock): string {
  return [
    table.columns.map((column) => tsvValue(column.label)).join("\t"),
    ...table.rows.map((row) => table.columns.map((column) => tsvValue(row[column.key] ?? null)).join("\t")),
  ].join("\n");
}

export function rowToTsv(table: TableBlock, row: Record<string, TableCellValue>): string {
  return [
    table.columns.map((column) => tsvValue(column.label)).join("\t"),
    table.columns.map((column) => tsvValue(row[column.key] ?? null)).join("\t"),
  ].join("\n");
}

export async function copyText(value: string): Promise<void> {
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

function csvValue(value: TableCellValue): string {
  if (value === null) return "";
  const text = String(value);
  return /[",\r\n]/.test(text) ? `"${text.replace(/"/g, '""')}"` : text;
}

export function tableToCsv(table: TableBlock, columnKeys: string[]): string {
  const selectedColumns = table.columns.filter((column) => columnKeys.includes(column.key));
  return [
    selectedColumns.map((column) => csvValue(column.label)).join(","),
    ...table.rows.map((row) => selectedColumns.map((column) => csvValue(row[column.key] ?? null)).join(",")),
  ].join("\r\n");
}

export function exportTableCsv(table: TableBlock, columnKeys: string[]): void {
  const blob = new Blob([`\uFEFF${tableToCsv(table, columnKeys)}`], { type: "text/csv;charset=utf-8" });
  const url = URL.createObjectURL(blob);
  const link = document.createElement("a");
  const date = new Date().toISOString().slice(0, 10);
  const title = table.title.trim().toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "") || "analytics-table";
  link.href = url;
  link.download = `${title}-${date}.csv`;
  document.body.appendChild(link);
  link.click();
  link.remove();
  window.setTimeout(() => URL.revokeObjectURL(url), 0);
}
