//! Test to see generated schemas

#[cfg(test)]
mod tests {
    //use crate::tool::builtin::data_source::{DataSourceInput, DataSourceOperation};
    use crate::tool::builtin::send_message::SendMessageInput;
    use crate::tool::builtin::{MessageTarget, RecallInput, RecallOp, TargetType};
    use schemars::schema_for;

    #[test]
    fn test_message_target_schema() {
        let schema = schema_for!(MessageTarget);
        let json = serde_json::to_string_pretty(&schema).unwrap();
        println!("MessageTarget schema:\n{}", json);
    }

    #[test]
    fn test_target_type_schema() {
        let schema = schema_for!(TargetType);
        let json = serde_json::to_string_pretty(&schema).unwrap();
        println!("TargetType schema:\n{}", json);

        // Check if it contains oneOf
        assert!(
            !json.contains("oneOf"),
            "TargetType should not generate oneOf schema"
        );
    }

    #[test]
    fn test_send_message_input_schema() {
        let schema = schema_for!(SendMessageInput);
        let json = serde_json::to_string_pretty(&schema).unwrap();
        println!("SendMessageInput schema:\n{}", json);
    }

    #[test]
    fn test_recall_op_schema() {
        let schema = schema_for!(RecallOp);
        let json = serde_json::to_string_pretty(&schema).unwrap();
        println!("RecallOp schema:\n{}", json);

        // Check if it contains oneOf
        if json.contains("oneOf") {
            eprintln!("WARNING: RecallOp generates oneOf schema!");
            eprintln!("This will cause issues with Gemini API");
        }
    }

    #[test]
    fn test_recall_input_schema() {
        let schema = schema_for!(RecallInput);
        let json = serde_json::to_string_pretty(&schema).unwrap();
        println!("RecallInput schema:\n{}", json);

        // Check for problematic patterns
        if json.contains("oneOf") {
            eprintln!("WARNING: RecallInput contains oneOf!");
        }
        if json.contains("const") {
            eprintln!("WARNING: RecallInput contains const!");
        }
    }

    // until data sources are reworked
    // #[test]
    // fn test_data_source_operation_schema() {
    //     let schema = schema_for!(DataSourceOperation);
    //     let json = serde_json::to_string_pretty(&schema).unwrap();
    //     println!("DataSourceOperation schema:\n{}", json);

    //     // Check if it contains oneOf
    //     if json.contains("oneOf") {
    //         eprintln!("WARNING: DataSourceOperation generates oneOf schema!");
    //         eprintln!("This will cause issues with Gemini API");
    //     }
    // }

    // #[test]
    // fn test_data_source_input_schema() {
    //     let schema = schema_for!(DataSourceInput);
    //     let json = serde_json::to_string_pretty(&schema).unwrap();
    //     println!("DataSourceInput schema:\n{}", json);

    //     // Check for problematic patterns
    //     if json.contains("oneOf") {
    //         eprintln!("WARNING: DataSourceInput contains oneOf!");
    //     }
    //     if json.contains("const") {
    //         eprintln!("WARNING: DataSourceInput contains const!");
    //     }

    //     // Check that optional fields are properly marked
    //     assert!(
    //         !json.contains(r#"null"#),
    //         "We should not have any null values for optional fields, instead they should be optional (i.e. not listed under \"required\".)"
    //     );
    // }
}
