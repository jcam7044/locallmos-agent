import type React from "react";

export const label: React.CSSProperties = { color: "#64748b", fontSize: 12 };

export const card: React.CSSProperties = {
  border: "1px solid #1f2937",
  background: "#131926",
  borderRadius: 12,
  padding: 14,
};

export const inputStyle: React.CSSProperties = {
  width: "100%",
  boxSizing: "border-box",
  marginTop: 8,
  padding: "8px 12px",
  borderRadius: 8,
  border: "1px solid #1f2937",
  background: "#0b0f17",
  color: "#e2e8f0",
  outline: "none",
};

export const buttonStyle: React.CSSProperties = {
  padding: "8px 14px",
  borderRadius: 8,
  border: "none",
  background: "#38bdf8",
  color: "#0f172a",
  fontWeight: 600,
  cursor: "pointer",
};

export const secondaryButton: React.CSSProperties = {
  padding: "6px 12px",
  borderRadius: 8,
  border: "1px solid #1f2937",
  background: "transparent",
  color: "#e2e8f0",
  fontSize: 12,
  cursor: "pointer",
};
