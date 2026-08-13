export type ChatRole = "user" | "assistant";

export interface TextMessageContent {
  type: "text";
  text: string;
  tone?: "default" | "error";
}

export type TableColumnType =
  | "string"
  | "number"
  | "currency"
  | "date"
  | "datetime"
  | "percentage";

export type TableCellValue = string | number | boolean | null;

export interface TableColumn {
  key: string;
  label: string;
  type: TableColumnType;
}

export interface TableBlock {
  id: string;
  title: string;
  columns: TableColumn[];
  rows: Array<Record<string, TableCellValue>>;
  rowCount: number;
  truncated: boolean;
}

export type ChartType = "bar" | "horizontal_bar" | "line";

export interface ChartBlock {
  id: string;
  type: ChartType;
  title: string;
  datasetId: string;
  xKey: string;
  yKey: string;
  xLabel: string;
  yLabel: string;
}

export interface WarningBlock {
  message: string;
}

export interface AgentResponse {
  answer: string;
  tables?: TableBlock[];
  charts?: ChartBlock[];
  warnings?: WarningBlock[];
  dataAsOf?: string;
}

export interface AnalyticsMessageContent {
  type: "analytics";
  response: AgentResponse;
}

export type ChatMessageContent = TextMessageContent | AnalyticsMessageContent;

export interface ChatMessage {
  id: string;
  role: ChatRole;
  content: ChatMessageContent;
}
