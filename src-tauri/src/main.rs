fn main() {
    // Before anything else runs (no thread, no runtime yet): may replace the process image.
    ai_task_manager::scrub_inherited_session();
    ai_task_manager::run();
}
