import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { applyStoredInterface } from "./interface";
import "./styles.css";

// Edit > Preferences > Interface, applied before the first paint so a
// light theme never flashes dark.
applyStoredInterface();

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
