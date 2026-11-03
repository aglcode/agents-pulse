import { useEffect } from "react";
import { Overview } from "./features/overview/Overview";
import { startEngineSync } from "./store/engineStore";

function App() {
  useEffect(() => startEngineSync(), []);

  return (
    <div className="min-h-screen bg-white text-zinc-900 dark:bg-zinc-950 dark:text-zinc-100">
      <Overview />
    </div>
  );
}

export default App;
