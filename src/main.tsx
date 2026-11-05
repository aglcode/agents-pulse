import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { installReleaseContextMenuGuard } from "./app/contextMenu";
import "./styles/index.css";

installReleaseContextMenuGuard();

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
