import { Suspense } from "react";
import Workspace from "@/components/workspace";

export default function Page() {
  return (
    <Suspense fallback={<main className="boot">Opening Race Lab…</main>}>
      <Workspace />
    </Suspense>
  );
}
