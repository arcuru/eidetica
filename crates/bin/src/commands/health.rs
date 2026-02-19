//! Health check command - checks a running Eidetica server.

use std::time::Duration;

use crate::cli::HealthArgs;

/// Run the health check command
pub async fn run(args: &HealthArgs) -> Result<(), Box<dyn std::error::Error>> {
    let base = args.url.trim_end_matches('/');
    let url = if base.ends_with("/health") {
        base.to_string()
    } else {
        format!("{base}/health")
    };
    let timeout = Duration::from_secs(args.timeout);

    let client = reqwest::Client::builder().timeout(timeout).build()?;

    match client.get(&url).send().await {
        Ok(response) if response.status().is_success() => {
            let body: serde_json::Value = response.json().await?;
            let status = body.get("status").and_then(|s| s.as_str()).unwrap_or("");
            if status == "healthy" {
                println!("healthy: {}", body);
                Ok(())
            } else {
                eprintln!("unhealthy: server returned status {}", status);
                std::process::exit(1);
            }
        }
        Ok(response) => {
            eprintln!(
                "unhealthy: server returned HTTP status {}",
                response.status()
            );
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("unhealthy: failed to connect to {}: {}", url, e);
            std::process::exit(1);
        }
    }
}
