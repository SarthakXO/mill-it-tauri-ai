import { useMemo, useState } from "react";
import type { TableBlock, TableCellValue, TableColumn } from "../../../types/chat";
import { isNumericColumn } from "./tableUtils";

export type SortDirection = "ascending" | "descending";

export interface SortState {
  key: string;
  direction: SortDirection;
}

export function useTableSort(table: TableBlock) {
  const [sort, setSort] = useState<SortState | null>(null);
  const rows = useMemo(() => {
    const indexedRows = table.rows.map((row, originalIndex) => ({ row, originalIndex }));
    if (!sort) return indexedRows;
    const column = table.columns.find((candidate) => candidate.key === sort.key);
    if (!column) return indexedRows;
    return [...indexedRows].sort((left, right) => {
      const leftValue = left.row[column.key] ?? null;
      const rightValue = right.row[column.key] ?? null;
      const leftMissing = leftValue === null || leftValue === "";
      const rightMissing = rightValue === null || rightValue === "";
      if (leftMissing || rightMissing) {
        if (leftMissing && rightMissing) return left.originalIndex - right.originalIndex;
        return leftMissing ? 1 : -1;
      }
      const comparison = compareValues(leftValue, rightValue, column);
      if (comparison === 0) return left.originalIndex - right.originalIndex;
      return sort.direction === "ascending" ? comparison : -comparison;
    });
  }, [sort, table.columns, table.rows]);

  const toggleSort = (key: string) => {
    setSort((current) => {
      if (!current || current.key !== key) return { key, direction: "ascending" };
      if (current.direction === "ascending") return { key, direction: "descending" };
      return null;
    });
  };
  return { rows, sort, toggleSort };
}

function compareValues(left: TableCellValue, right: TableCellValue, column: TableColumn): number {
  if (isNumericColumn(column)) {
    const leftNumber = typeof left === "number" ? left : Number(left);
    const rightNumber = typeof right === "number" ? right : Number(right);
    if (Number.isFinite(leftNumber) && Number.isFinite(rightNumber)) return leftNumber - rightNumber;
  }
  if (column.type === "date" || column.type === "datetime") {
    const leftDate = new Date(String(left)).getTime();
    const rightDate = new Date(String(right)).getTime();
    if (Number.isFinite(leftDate) && Number.isFinite(rightDate)) return leftDate - rightDate;
  }
  return String(left).localeCompare(String(right), undefined, { numeric: true, sensitivity: "base" });
}
