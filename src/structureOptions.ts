import type { Driver } from "./types";

/** Common types offered while editing a column's type. Any other type can still be typed. */
export const DATA_TYPES: Record<Driver, string[]> = {
  mysql: [
    "int",
    "int unsigned",
    "bigint",
    "bigint unsigned",
    "smallint",
    "tinyint",
    "tinyint(1)",
    "decimal(10,2)",
    "float",
    "double",
    "varchar(255)",
    "char(36)",
    "text",
    "mediumtext",
    "longtext",
    "json",
    "date",
    "datetime",
    "timestamp",
    "time",
    "year",
    "binary(16)",
    "blob",
    "longblob",
  ],
  postgres: [
    "integer",
    "bigint",
    "smallint",
    "serial",
    "bigserial",
    "numeric(10,2)",
    "real",
    "double precision",
    "boolean",
    "varchar(255)",
    "text",
    "uuid",
    "json",
    "jsonb",
    "date",
    "timestamp",
    "timestamptz",
    "time",
    "interval",
    "bytea",
    "inet",
    "text[]",
    "integer[]",
  ],
  sqlite: ["INTEGER", "TEXT", "REAL", "NUMERIC", "BLOB", "BOOLEAN", "DATETIME", "VARCHAR(255)"],
};

/** Common default expressions. Defaults are SQL, so text needs its quotes. */
export const DEFAULT_VALUES: Record<Driver, string[]> = {
  mysql: ["NULL", "CURRENT_TIMESTAMP", "0", "1", "''", "(uuid())"],
  postgres: ["NULL", "now()", "CURRENT_TIMESTAMP", "gen_random_uuid()", "0", "true", "false", "''"],
  sqlite: ["NULL", "CURRENT_TIMESTAMP", "0", "1", "''"],
};

export function quoteIdentifier(driver: Driver, name: string) {
  return driver === "mysql" ? `\`${name.replace(/`/g, "``")}\`` : `"${name.replace(/"/g, '""')}"`;
}
