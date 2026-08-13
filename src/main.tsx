import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { ResourcesWindow } from "./resources/ResourcesWindow";

const view = new URLSearchParams(window.location.search).get("view");

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    {view === "resources" ? <ResourcesWindow /> : <App />}
  </StrictMode>,
);
