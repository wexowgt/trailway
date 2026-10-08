import { ProjectCanvas } from "@/components/ProjectCanvas";

export const metadata = { title: "Architecture - Trailway" };

export default async function ProjectPage({ params }: { params: Promise<{ id: string }> }) {
  const { id } = await params;
  return <ProjectCanvas projectId={id} />;
}
