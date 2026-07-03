import { createRoot } from "react-dom/client";
import "./app.css";
import { WebUI } from "./app/WebUI";

const root = document.getElementById("root");
if (root) {
  createRoot(root).render(<WebUI />);
}
