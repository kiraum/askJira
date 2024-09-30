use env_logger::Env;
use futures::stream::StreamExt;
use indicatif::{ProgressBar, ProgressStyle};
use log::{debug, warn};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::Client;
use serde_json::{json, Value};
use std::env;
use std::error::Error;
use std::process;
use std::time::Duration;
use structopt::StructOpt;

#[derive(StructOpt, Debug)]
#[structopt(
    name = "askJira",
    about = "Ask Cody a question or a question about Jira tickets if JQL is provided."
)]
struct Opt {
    #[structopt(long, help = "The message to send to Cody")]
    message: Option<String>,

    #[structopt(long, help = "JQL query to search Jira tickets")]
    jql: Option<String>,

    #[structopt(
        long,
        default_value = "1000",
        help = "Maximum number of issues to fetch"
    )]
    max_issues: usize,

    #[structopt(
        long,
        default_value = "100",
        help = "Maximum number of results per Jira API call"
    )]
    max_results: usize,

    #[structopt(long, help = "Enable debug mode")]
    debug: bool,

    #[structopt(long, help = "List available models")]
    list_models: bool,

    #[structopt(long, help = "Set the model to use")]
    set_model: Option<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let opt = Opt::from_args();

    let env = if opt.debug {
        Env::default().filter_or("RUST_LOG", "debug")
    } else {
        Env::default().filter_or("RUST_LOG", "info")
    };
    env_logger::init_from_env(env);

    debug!(
        "Command line arguments: {:?}",
        std::env::args().collect::<Vec<String>>()
    );
    debug!("Parsed command line arguments: {:?}", opt);

    let access_token = env::var("SRC_ACCESS_TOKEN")
        .expect("Error: SRC_ACCESS_TOKEN environment variable is not set.");
    let endpoint =
        env::var("SRC_ENDPOINT").expect("Error: SRC_ENDPOINT environment variable is not set.");

    if opt.list_models {
        list_available_models(&endpoint).await?;
        return Ok(());
    }

    if opt.message.is_none() && opt.jql.is_none() {
        warn!("No message or JQL provided. Printing help.");
        Opt::clap().print_help().unwrap();
        println!();
        process::exit(0);
    }

    let chat_completions_url = format!(
        "{}/.api/completions/stream?api-version=1&client-name=defaultclient&client-version=6.0.0'",
        endpoint
    );

    debug!("Chat completions URL: {}", chat_completions_url);

    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("token {}", access_token))?,
    );

    debug!("Headers set up: {:?}", headers);

    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::default_spinner()
            .tick_strings(&["|", "/", "-", "\\"])
            .template("{spinner:.green} {msg}"),
    );
    pb.enable_steady_tick(100);
    pb.set_message("Processing...");

    let model = opt
        .set_model
        .unwrap_or_else(|| "anthropic::2023-06-01::claude-3.5-sonnet".to_string());

    let result = match (opt.jql, opt.message) {
        (Some(jql), Some(message)) => {
            debug!(
                "Both JQL and message provided. JQL: {:?}, Message: {:?}",
                jql, message
            );
            let jira_token =
                env::var("JIRA_TOKEN").map_err(|_| "JIRA_TOKEN environment variable is not set")?;
            let jira_host =
                env::var("JIRA_HOST").map_err(|_| "JIRA_HOST environment variable is not set")?;

            debug!("JIRA_HOST = {}", jira_host);
            debug!("JQL Query = {:?}", jql);

            let jira_data = fetch_jira_data(
                &jira_host,
                &jira_token,
                &jql,
                opt.max_issues,
                opt.max_results,
            )
            .await?;
            debug!("Jira data fetched, length: {} characters", jira_data.len());

            let batch_summaries =
                process_jira_data(&message, jira_data, &chat_completions_url, &headers, &model)
                    .await?;
            debug!(
                "Jira data processed, {} batch summaries",
                batch_summaries.len()
            );

            let aggregated_summaries = batch_summaries.join("\n\n--- Next Batch Summary ---\n\n");

            let final_query = format!(
                "Original question(s):\n{}\n\nSummaries of Jira data batches:\n{}\n\nBased on these batch summaries, please provide a comprehensive and cohesive answer to the original question(s). Synthesize the information from all batch summaries, highlighting key points, trends, and insights relevant to the question(s).",
                message,
                aggregated_summaries
            );

            debug!("Final query length: {} characters", final_query.len());
            let final_answer =
                cody_chat(&final_query, &chat_completions_url, &headers, &model).await?;
            println!("Answer:\n{}", final_answer);
            Ok(())
        }
        (None, Some(message)) => {
            debug!("Only message provided, no JQL. Message: {:?}", message);
            let answer = cody_chat(&message, &chat_completions_url, &headers, &model).await?;
            pb.finish_and_clear();
            println!("Answer:\n{}", answer);
            Ok(())
        }
        (Some(_), None) => {
            warn!("JQL provided without a message");
            Err("When using --jql, --message is also required".into())
        }
        (None, None) => {
            warn!("Neither JQL nor message provided");
            Err("Either --message or both --message and --jql must be provided".into())
        }
    };

    pb.finish_and_clear();
    result
}

async fn list_available_models(endpoint: &str) -> Result<(), Box<dyn Error>> {
    let models_url = format!("{}/.api/llm/models", endpoint);

    let client = Client::new();
    let response = client
        .get(&models_url)
        .header("Accept", "application/json")
        .send()
        .await?;

    let models: Value = response.json().await?;
    println!("Available models:");
    for model in models["data"].as_array().unwrap() {
        println!("- ID: {}", model["id"].as_str().unwrap());
        println!("  Owned by: {}", model["owned_by"].as_str().unwrap());
        println!();
    }

    Ok(())
}

async fn fetch_jira_data(
    host: &str,
    token: &str,
    jql: &str,
    max_issues: usize,
    max_results: usize,
) -> Result<String, Box<dyn Error>> {
    let client = Client::builder()
        .timeout(Duration::from_secs(300))
        .build()?;

    let search_url = format!("{}/rest/api/2/search", host);

    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", token))?,
    );
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

    debug!("Sending search request to {}", search_url);

    let mut start_at = 0;
    let mut all_issues = Vec::new();

    loop {
        let search_response = client
            .get(&search_url)
            .headers(headers.clone())
            .query(&[
                ("jql", jql),
                ("startAt", &start_at.to_string()),
                ("maxResults", &max_results.to_string()),
            ])
            .send()
            .await?;

        debug!("Search response status: {}", search_response.status());

        let search_status = search_response.status();
        let search_body = search_response.text().await?;

        if !search_status.is_success() {
            debug!("Search error response body: {}", search_body);
            return Err(format!(
                "Jira search API request failed with status: {}. Body: {}",
                search_status, search_body
            )
            .into());
        }

        let search_data: Value = serde_json::from_str(&search_body)?;
        let issues = search_data["issues"].as_array().ok_or("No issues found")?;
        all_issues.extend_from_slice(issues);

        let total_issues = search_data["total"].as_u64().unwrap_or(0) as usize;
        start_at += issues.len();

        debug!(
            "Fetched {} issues out of {}",
            all_issues.len(),
            total_issues
        );

        if start_at >= total_issues || all_issues.len() >= max_issues {
            break;
        }
    }

    debug!("Total number of issues fetched: {}", all_issues.len());

    if all_issues.len() >= max_issues {
        warn!(
            "Reached max_issues limit ({}). Not all Jira data was fetched.",
            max_issues
        );
    }

    let mut aggregated_data = Vec::new();
    let mut aggregated_count = 0;

    for issue in all_issues.iter().take(max_issues) {
        let key = issue["key"].as_str().ok_or("Issue key not found")?;
        let comments_url = format!("{}/rest/api/2/issue/{}/comment", host, key);

        let comments_response = client
            .get(&comments_url)
            .headers(headers.clone())
            .send()
            .await?;

        let comments_body = comments_response.text().await?;
        let comments_data: Value = serde_json::from_str(&comments_body)?;

        let mut issue_data = json!({
            "key": key,
            "fields": issue["fields"],
            "comments": comments_data["comments"]
        });

        remove_null_values(&mut issue_data);
        aggregated_data.push(issue_data);
        aggregated_count += 1;

        debug!("Aggregated issue {}/{}", aggregated_count, max_issues);
    }

    debug!("Total issues aggregated: {}", aggregated_count);

    let result = serde_json::to_string_pretty(&aggregated_data)?;
    debug!("Aggregated data length: {} characters", result.len());

    Ok(result)
}

fn remove_null_values(value: &mut Value) {
    if let Value::Object(map) = value {
        map.retain(|_, v| !v.is_null());
        for v in map.values_mut() {
            remove_null_values(v);
        }
    } else if let Value::Array(array) = value {
        for v in array {
            remove_null_values(v);
        }
    }
}

async fn process_jira_data(
    message: &str,
    jira_data: String,
    chat_completions_url: &str,
    headers: &HeaderMap,
    model: &str,
) -> Result<Vec<String>, Box<dyn Error>> {
    const BATCH_SIZE: usize = 200_000;

    let jira_data: Value = serde_json::from_str(&jira_data)?;
    if jira_data.as_array().unwrap().is_empty() {
        warn!("No Jira data found");
        return Ok(vec![format!("Context: No Jira data found.")]);
    }

    let mut batches = Vec::new();
    let mut current_batch = Vec::new();
    let mut current_batch_size = 0;

    for issue in jira_data.as_array().unwrap() {
        let issue_str = serde_json::to_string(issue)?;
        if current_batch_size + issue_str.len() > BATCH_SIZE && !current_batch.is_empty() {
            batches.push(current_batch);
            current_batch = Vec::new();
            current_batch_size = 0;
        }
        current_batch.push(issue.clone());
        current_batch_size += issue_str.len();
    }

    if !current_batch.is_empty() {
        batches.push(current_batch);
    }

    let total_batches = batches.len();
    debug!("Created {} batches of Jira data", total_batches);

    let mut batch_summaries = Vec::new();
    for (i, batch) in batches.into_iter().enumerate() {
        let batch_str = serde_json::to_string(&batch)?;
        let batch_query = format!(
            "Question(s):\n{}\n\nJira data:\n{}\n\nBased on Jira data, please provide a comprehensive answer for the question(s).",
            message,
            batch_str
        );

        debug!(
            "Processing batch {} out of {} of size {} characters",
            i + 1,
            total_batches,
            batch_query.len()
        );
        let batch_summary = cody_chat(&batch_query, chat_completions_url, headers, model).await?;
        debug!(
            "Processed batch {} out of {} answer:\n{}",
            i + 1,
            total_batches,
            batch_summary
        );
        batch_summaries.push(batch_summary);
    }

    Ok(batch_summaries)
}

async fn cody_chat(
    query: &str,
    chat_completions_url: &str,
    headers: &HeaderMap,
    model: &str,
) -> Result<String, Box<dyn Error>> {
    let final_prompt = format!(
        r#"
    You are given the following query:
    {}
    Please provide a concise and informative answer.
    "#,
        query
    );

    debug!(
        "Sending chat query of length: {} characters",
        final_prompt.len()
    );
    let response = chat_completions(&final_prompt, chat_completions_url, headers, model).await?;
    debug!(
        "Received chat response of length: {} characters",
        response.len()
    );
    Ok(response)
}

async fn chat_completions(
    query: &str,
    chat_completions_url: &str,
    headers: &HeaderMap,
    model: &str,
) -> Result<String, Box<dyn Error>> {
    let messages = if chat_completions_url.contains("https://sourcegraph.com") {
        json!([{"speaker": "human", "text": query}])
    } else {
        json!([{"role": "user", "content": query}])
    };

    let data = json!({
        "maxTokensToSample": 4000,
        "messages": messages,
        "model": model,
        "temperature": 0.2,
        "topK": -1,
        "topP": -1,
        "stream": true,
    });

    let client = Client::builder()
        .timeout(Duration::from_secs(300))
        .build()?;

    debug!(
        "Sending chat completion request to {}",
        chat_completions_url
    );
    let mut response = client
        .post(chat_completions_url)
        .headers(headers.clone())
        .json(&data)
        .send()
        .await?
        .bytes_stream();

    let mut last_response = String::new();
    let mut buffer = String::new();

    while let Some(chunk) = response.next().await {
        let chunk = chunk?;
        buffer.push_str(&String::from_utf8_lossy(&chunk));

        while let Some(pos) = buffer.find('\n') {
            let line = buffer.drain(..=pos).collect::<String>();
            if line.starts_with("data: ") {
                let data = line.trim_start_matches("data: ");
                if data != "[DONE]" {
                    if let Ok(event_data) = serde_json::from_str::<Value>(data) {
                        if let Some(completion) = event_data["completion"].as_str() {
                            last_response = completion.to_string();
                        }
                    }
                }
            }
        }
    }

    debug!(
        "Chat completion response received, length: {} characters",
        last_response.len()
    );
    debug!("Response content: {}", last_response);
    Ok(last_response)
}
