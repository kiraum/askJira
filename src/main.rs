use env_logger::Env;
use futures::future::join_all;
use indicatif::{ProgressBar, ProgressStyle};
use log::{debug, warn};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::Client;
use serde_json::{json, Value};
use std::env;
use std::error::Error;
use std::process;
use std::sync::Arc;
use std::time::Duration;
use structopt::StructOpt;
use tokio::sync::Semaphore;
use tokio::time::sleep;

/// Command-line options for the askJira application
#[derive(StructOpt, Debug)]
#[structopt(
    name = "askJira",
    about = "Ask Cody a question or a question about Jira tickets if JQL is provided."
)]
struct Opt {
    /// The message to send to Cody
    #[structopt(long, help = "The message to send to Cody")]
    message: Option<String>,

    /// JQL query to search Jira tickets
    #[structopt(long, help = "JQL query to search Jira tickets")]
    jql: Option<String>,

    /// Automatically generate JQL based on the message
    #[structopt(long, help = "Automatically generate JQL based on the message")]
    auto_jql: bool,

    /// Maximum number of issues to fetch
    #[structopt(
        long,
        default_value = "100000",
        help = "Maximum number of issues to fetch"
    )]
    max_issues: usize,

    /// Maximum number of results per Jira API call
    #[structopt(
        long,
        default_value = "100",
        help = "Maximum number of results per Jira API call"
    )]
    max_results: usize,

    /// Enable debug mode
    #[structopt(long, help = "Enable debug mode")]
    debug: bool,

    /// List available models
    #[structopt(long, help = "List available models")]
    list_models: bool,

    /// Set the model to use
    #[structopt(long, help = "Set the model to use")]
    set_model: Option<String>,

    /// Maximum number of tokens in the response
    #[structopt(
        long,
        default_value = "2000",
        help = "Maximum number of tokens in the response"
    )]
    max_tokens: usize,
}

/// Main function to run the askJira application
#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let opt = Opt::from_args();

    // Set up logging based on debug flag
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

    // Get environment variables
    let access_token = env::var("SRC_ACCESS_TOKEN")
        .expect("Error: SRC_ACCESS_TOKEN environment variable is not set.");
    let endpoint =
        env::var("SRC_ENDPOINT").expect("Error: SRC_ENDPOINT environment variable is not set.");

    // List available models if requested
    if opt.list_models {
        list_available_models(&endpoint, &access_token).await?;
        return Ok(());
    }

    // Set the model to use
    let model = if let Some(set_model) = opt.set_model {
        let available_models = fetch_available_models(&endpoint, &access_token).await?;
        if available_models.contains(&set_model) {
            set_model
        } else {
            return Err(format!(
                "Invalid model: {}. Use --list-models to see available models.",
                set_model
            )
            .into());
        }
    } else {
        "anthropic::2023-06-01::claude-3.5-sonnet".to_string()
    };

    // Check if message is provided
    if opt.message.is_none() {
        warn!("No message provided. Printing help.");
        Opt::clap().print_help().unwrap();
        println!();
        process::exit(0);
    }

    // Set up chat completions URL
    let chat_completions_url = format!("{}/.api/llm/chat/completions", endpoint);
    debug!("Chat completions URL: {}", chat_completions_url);

    // Set up headers
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("token {}", access_token))?,
    );
    headers.insert("Accept", HeaderValue::from_static("application/json"));
    debug!("Headers set up: {:?}", headers);

    // Set up progress bar
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::default_spinner()
            .tick_strings(&["|", "/", "-", "\\"])
            .template("{spinner:.green} {msg}"),
    );
    pb.enable_steady_tick(100);
    pb.set_message("Processing...");

    // Process the request based on provided options
    let result = match (opt.jql, opt.message, opt.auto_jql) {
        (Some(_), _, true) => Err("--auto-jql cannot be used with --jql".into()),
        (None, Some(message), true) => {
            debug!(
                "Automatically generating JQL based on message: {:?}",
                message
            );
            let jira_token =
                env::var("JIRA_TOKEN").expect("JIRA_TOKEN environment variable is not set");
            let jira_host =
                env::var("JIRA_HOST").expect("JIRA_HOST environment variable is not set");
            let jira_projects = fetch_jira_projects(&jira_host, &jira_token).await?;
            debug!("Jira projects: {:?}", jira_projects);
            let auto_generated_jql = auto_generate_jql(
                &message,
                &jira_projects,
                &chat_completions_url,
                &headers,
                &model,
                &jira_host,
                &jira_token,
                opt.max_tokens,
            )
            .await?;
            warn!("Auto-generated JQL: {}", auto_generated_jql);
            println!("Auto-generated JQL: {}", auto_generated_jql);
            debug!("Auto-generated JQL: {}", auto_generated_jql);
            process_jira_query(
                &message,
                &auto_generated_jql,
                &jira_host,
                &jira_token,
                opt.max_issues,
                opt.max_results,
                &chat_completions_url,
                &headers,
                &model,
                opt.max_tokens,
            )
            .await
        }
        (Some(jql), Some(message), false) => {
            let jira_token =
                env::var("JIRA_TOKEN").expect("JIRA_TOKEN environment variable is not set");
            let jira_host =
                env::var("JIRA_HOST").expect("JIRA_HOST environment variable is not set");
            process_jira_query(
                &message,
                &jql,
                &jira_host,
                &jira_token,
                opt.max_issues,
                opt.max_results,
                &chat_completions_url,
                &headers,
                &model,
                opt.max_tokens,
            )
            .await
        }
        (None, Some(message), false) => {
            debug!("Only message provided, no JQL. Message: {:?}", message);
            let answer = cody_chat(
                &message,
                &chat_completions_url,
                &headers,
                &model,
                opt.max_tokens,
            )
            .await?;
            pb.finish_and_clear();
            println!();
            println!("Answer:\n{}", answer);
            Ok(())
        }
        _ => {
            warn!("Invalid combination of options");
            Err(
                "Invalid combination of options. Use --message with either --jql or --auto-jql"
                    .into(),
            )
        }
    };

    pb.finish_and_clear();
    println!();
    result
}

async fn fetch_jira_projects(host: &str, token: &str) -> Result<Vec<String>, Box<dyn Error>> {
    let client = Client::new();
    let projects_url = format!("{}/rest/api/2/project", host);

    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", token))?,
    );

    let response = client.get(&projects_url).headers(headers).send().await?;
    let projects: Vec<Value> = response.json().await?;

    Ok(projects
        .into_iter()
        .filter_map(|p| p["key"].as_str().map(String::from))
        .collect())
}

#[allow(clippy::too_many_arguments)]
async fn auto_generate_jql(
    message: &str,
    projects: &[String],
    chat_completions_url: &str,
    headers: &HeaderMap,
    model: &str,
    jira_host: &str,
    jira_token: &str,
    max_tokens: usize,
) -> Result<String, Box<dyn Error>> {
    // Check if any project key is present in the message
    let found_project = projects.iter().find(|&project| {
        message
            .to_uppercase()
            .split_whitespace()
            .any(|word| word == project)
    });

    if let Some(project) = found_project {
        debug!("Found project in message: {}", project);
    } else {
        debug!("No exact project match found in message");
    }

    let prompt = if let Some(project) = found_project {
        // Fetch more information about the project
        let project_info = fetch_project_info(jira_host, jira_token, project).await?;
        debug!("Project information: {}", project_info);

        format!(
            "Given the following question and project information, generate a valid JQL query to search for relevant issues in the {} project. Use only the components, issue types, and fields that exist in the provided project information. Ensure the query is specific to the question asked, but avoid overly broad conditions. Provide only the JQL query without any explanation or additional clauses:\n\nQuestion: {}\n\nProject Information:\n{}",
            project,
            message,
            project_info
        )
    } else {
        format!(
            "Given the following question and list of Jira projects, generate a valid JQL query to search for relevant issues, avoid overly broad conditions that might lead to false positives. Provide only the JQL query without any explanation, LIMIT clause, labels clause, or anything else:\n\nQuestion: {}\n\nProjects: {}",
            message,
            projects.join(", ")
        )
    };

    let auto_generated_jql =
        cody_chat(&prompt, chat_completions_url, headers, model, max_tokens).await?;

    Ok(auto_generated_jql.trim().to_string())
}

async fn fetch_project_info(
    host: &str,
    token: &str,
    project_key: &str,
) -> Result<String, Box<dyn Error>> {
    let client = Client::new();
    let project_url = format!("{}/rest/api/2/project/{}", host, project_key);

    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", token))?,
    );

    let response = client.get(&project_url).headers(headers).send().await?;
    let project_data: Value = response.json().await?;

    Ok(project_data.to_string())
}

#[allow(clippy::too_many_arguments)]
async fn process_jira_query(
    message: &str,
    jql: &str,
    jira_host: &str,
    jira_token: &str,
    max_issues: usize,
    max_results: usize,
    chat_completions_url: &str,
    headers: &HeaderMap,
    model: &str,
    max_tokens: usize,
) -> Result<(), Box<dyn Error>> {
    debug!(
        "Processing Jira query. JQL: {:?}, Message: {:?}",
        jql, message
    );
    debug!("JIRA_HOST = {}", jira_host);
    debug!("JQL Query = {:?}", jql);

    let jira_data = fetch_jira_data(jira_host, jira_token, jql, max_issues, max_results).await?;
    debug!("Jira data fetched, length: {} characters", jira_data.len());

    let batch_summaries = process_jira_data(
        message,
        jira_data,
        chat_completions_url,
        headers,
        model,
        max_tokens,
    )
    .await?;
    debug!(
        "Jira data processed, {} batch summaries",
        batch_summaries.len()
    );

    if batch_summaries.is_empty() {
        println!("Answer: No Jira data found for the given query.");
        return Ok(());
    }

    let aggregated_summaries = batch_summaries.join("\n\n--- Next Batch Summary ---\n\n");

    let final_query = format!(
        "Original question(s):\n{}\n\nJira data batches:\n{}\n\nBased on these batches, please provide a comprehensive and cohesive answer to the original question(s). Synthesize the information from all batch summaries, highlighting key points, trends, and insights relevant to the question(s).",
        message,
        aggregated_summaries
    );

    debug!("Final query length: {} characters", final_query.len());
    let final_answer = cody_chat(
        &final_query,
        chat_completions_url,
        headers,
        model,
        max_tokens,
    )
    .await?;
    println!("Answer:\n{}", final_answer);
    Ok(())
}

/// Fetch available models from the API
async fn fetch_available_models(
    endpoint: &str,
    access_token: &str,
) -> Result<Vec<String>, Box<dyn Error>> {
    let models_url = format!("{}/.api/llm/models", endpoint);
    let client = Client::new();

    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", access_token))?,
    );
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

    let response = client.get(&models_url).headers(headers).send().await?;

    let response_text = response.text().await?;
    debug!("Raw response from models API: {}", response_text);

    let models: Value = serde_json::from_str(&response_text).map_err(|e| {
        format!(
            "Failed to parse JSON: {}. Raw response: {}",
            e, response_text
        )
    })?;

    let available_models = models["data"]
        .as_array()
        .ok_or("'data' field is not an array")?
        .iter()
        .filter_map(|model| model["id"].as_str().map(String::from))
        .collect();

    Ok(available_models)
}

/// List available models
async fn list_available_models(endpoint: &str, access_token: &str) -> Result<(), Box<dyn Error>> {
    let available_models = fetch_available_models(endpoint, access_token).await?;
    println!("Available models:");
    for model in available_models {
        println!("- {}", model);
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
        println!(
            "Note: Reached max_issues limit ({}). Not all Jira data was fetched.",
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

/// Remove null values from a JSON Value
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

/// Process Jira data by batching and summarizing
async fn process_jira_data(
    message: &str,
    jira_data: String,
    chat_completions_url: &str,
    headers: &HeaderMap,
    model: &str,
    max_tokens: usize,
) -> Result<Vec<String>, Box<dyn Error>> {
    /// batch limit due Cody input limit
    const BATCH_SIZE: usize = 200_000;
    /// safe limit to protect the APIs from overloading
    const MAX_CONCURRENT_TASKS: usize = 50;

    let jira_data: Value = serde_json::from_str(&jira_data)?;
    if jira_data.as_array().unwrap().is_empty() {
        warn!("No Jira data found");
        return Ok(Vec::new());
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

    let semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT_TASKS));
    let batch_futures = batches.into_iter().enumerate().map(|(i, batch)| {
        let batch_str = serde_json::to_string(&batch).unwrap();
        let batch_query = format!(
            "Question(s):\n{}\n\nJira data:\n{}\n\nBased on Jira data, please provide a comprehensive answer for the question(s).",
            message,
            batch_str
        );

        let chat_completions_url = chat_completions_url.to_string();
        let headers = headers.clone();
        let model = model.to_string();
        let sem = semaphore.clone();

        async move {
            let _permit = sem.acquire().await.unwrap();
            debug!(
                "Processing batch {} out of {} of size {} characters",
                i + 1,
                total_batches,
                batch_query.len()
            );
            let batch_summary = cody_chat(&batch_query, &chat_completions_url, &headers, &model, max_tokens).await?;
            debug!(
                "Processed batch {} out of {} answer:\n{}",
                i + 1,
                total_batches,
                batch_summary
            );
            Ok::<String, Box<dyn Error>>(batch_summary)
        }
    });

    let batch_results = join_all(batch_futures).await;
    let batch_summaries: Result<Vec<String>, Box<dyn Error>> = batch_results
        .into_iter()
        .collect::<Result<Vec<String>, Box<dyn Error>>>();

    batch_summaries
}

/// Send a chat query to Cody and get a response
async fn cody_chat(
    query: &str,
    chat_completions_url: &str,
    headers: &HeaderMap,
    model: &str,
    max_tokens: usize,
) -> Result<String, Box<dyn Error>> {
    let messages = json!([
        {
            "role": "user",
            "content": query
        }
    ]);

    debug!("Sending chat query of length: {} characters", query.len());

    const MAX_RETRIES: u32 = 5;
    const INITIAL_BACKOFF: u64 = 1000; // 1s

    for attempt in 0..MAX_RETRIES {
        match chat_completions(
            messages.clone(),
            chat_completions_url,
            headers,
            model,
            max_tokens,
        )
        .await
        {
            Ok(response) => {
                debug!(
                    "Received chat response of length: {} characters",
                    response.len()
                );
                return Ok(response);
            }
            Err(e) => {
                if attempt < MAX_RETRIES - 1 {
                    let backoff = INITIAL_BACKOFF * 2u64.pow(attempt);
                    warn!(
                        "Attempt {} failed: {}. Retrying in {} ms...",
                        attempt + 1,
                        e,
                        backoff
                    );
                    sleep(Duration::from_millis(backoff)).await;
                } else {
                    return Err(e);
                }
            }
        }
    }

    Err("Max retries reached".into())
}

/// Send a chat completion request and process the response
async fn chat_completions(
    messages: serde_json::Value,
    chat_completions_url: &str,
    headers: &HeaderMap,
    model: &str,
    max_tokens: usize,
) -> Result<String, Box<dyn Error>> {
    let data = json!({
        "model": model,
        "max_tokens": max_tokens,
        "messages": messages,
        "stream": false
    });

    let client = Client::builder()
        .timeout(Duration::from_secs(300))
        .build()?;

    debug!(
        "Sending chat completion request to {}",
        chat_completions_url
    );
    debug!("Headers being sent: {:?}", headers);
    let response = client
        .post(chat_completions_url)
        .headers(headers.clone())
        .json(&data)
        .send()
        .await?;

    debug!("Response status: {:?}", response.status());

    if !response.status().is_success() {
        return Err(format!("HTTP error: {}", response.status()).into());
    }

    let response_text = response.text().await?;
    debug!("Raw response: {}", response_text);

    let response_body: Value = serde_json::from_str(&response_text).map_err(|e| {
        format!(
            "Failed to parse JSON: {}. Raw response: {}",
            e, response_text
        )
    })?;

    debug!("Chat completion response received: {:?}", response_body);

    let content = response_body["choices"][0]["message"]["content"]
        .as_str()
        .ok_or("Failed to extract content from response")?
        .to_string();

    Ok(content)
}
