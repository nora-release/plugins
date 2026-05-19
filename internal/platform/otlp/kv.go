package otlp

import commonv1 "go.opentelemetry.io/proto/otlp/common/v1"

func KVString(key, value string) *commonv1.KeyValue {
	return &commonv1.KeyValue{
		Key: key,
		Value: &commonv1.AnyValue{
			Value: &commonv1.AnyValue_StringValue{StringValue: value},
		},
	}
}

func KVInt(key string, value int64) *commonv1.KeyValue {
	return &commonv1.KeyValue{
		Key: key,
		Value: &commonv1.AnyValue{
			Value: &commonv1.AnyValue_IntValue{IntValue: value},
		},
	}
}
